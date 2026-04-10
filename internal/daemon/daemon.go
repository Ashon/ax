package daemon

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"log"
	"net"
	"os"
	"path/filepath"
	"sync"
)

const DefaultSocketPath = "~/.local/state/amux/daemon.sock"

func ExpandSocketPath(path string) string {
	if len(path) > 0 && path[0] == '~' {
		home, _ := os.UserHomeDir()
		path = filepath.Join(home, path[1:])
	}
	return path
}

type Daemon struct {
	socketPath   string
	registry     *Registry
	queue        *MessageQueue
	history      *History
	sharedValues map[string]string
	sharedMu     sync.RWMutex
	listener     net.Listener
	logger       *log.Logger
}

func New(socketPath string) *Daemon {
	sp := ExpandSocketPath(socketPath)
	stateDir := filepath.Dir(sp)
	return &Daemon{
		socketPath:   sp,
		registry:     NewRegistry(),
		queue:        NewMessageQueue(),
		history:      NewHistory(stateDir, 500),
		sharedValues: make(map[string]string),
		logger:       log.New(os.Stderr, "[amux-daemon] ", log.LstdFlags),
	}
}

func (d *Daemon) Run(ctx context.Context) error {
	// Ensure socket directory exists
	dir := filepath.Dir(d.socketPath)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return fmt.Errorf("create socket dir: %w", err)
	}

	// Remove stale socket file
	os.Remove(d.socketPath)

	ln, err := net.Listen("unix", d.socketPath)
	if err != nil {
		return fmt.Errorf("listen: %w", err)
	}
	d.listener = ln
	d.logger.Printf("listening on %s", d.socketPath)

	// Write PID file
	pidPath := filepath.Join(dir, "daemon.pid")
	os.WriteFile(pidPath, []byte(fmt.Sprintf("%d", os.Getpid())), 0o644)
	defer os.Remove(pidPath)

	go func() {
		<-ctx.Done()
		ln.Close()
	}()

	for {
		conn, err := ln.Accept()
		if err != nil {
			select {
			case <-ctx.Done():
				d.logger.Println("shutting down")
				return nil
			default:
				d.logger.Printf("accept error: %v", err)
				continue
			}
		}
		go d.handleConn(conn)
	}
}

func (d *Daemon) handleConn(conn net.Conn) {
	defer conn.Close()

	var workspace string
	scanner := bufio.NewScanner(conn)
	scanner.Buffer(make([]byte, 1024*1024), 1024*1024) // 1MB max message

	for scanner.Scan() {
		var env Envelope
		if err := json.Unmarshal(scanner.Bytes(), &env); err != nil {
			d.logger.Printf("decode error: %v", err)
			continue
		}

		resp, err := d.handleEnvelope(conn, &env, &workspace)
		if err != nil {
			d.logger.Printf("handle error [%s]: %v", env.Type, err)
			errEnv, _ := NewErrorEnvelope(env.ID, err.Error())
			d.writeEnvelope(conn, errEnv)
			continue
		}
		if resp != nil {
			d.writeEnvelope(conn, resp)
		}
	}

	// Cleanup on disconnect
	if workspace != "" {
		d.logger.Printf("workspace %q disconnected", workspace)
		d.registry.Unregister(workspace)
	}
}

func (d *Daemon) handleEnvelope(conn net.Conn, env *Envelope, workspace *string) (*Envelope, error) {
	switch env.Type {
	case MsgRegister:
		var p RegisterPayload
		if err := env.DecodePayload(&p); err != nil {
			return nil, fmt.Errorf("decode register: %w", err)
		}
		*workspace = p.Workspace
		d.registry.Register(p.Workspace, p.Dir, p.Description, conn)
		d.logger.Printf("registered workspace %q", p.Workspace)
		return NewResponseEnvelope(env.ID, map[string]string{"status": "registered"})

	case MsgUnregister:
		if *workspace != "" {
			d.registry.Unregister(*workspace)
			d.logger.Printf("unregistered workspace %q", *workspace)
			*workspace = ""
		}
		return NewResponseEnvelope(env.ID, map[string]string{"status": "unregistered"})

	case MsgSendMessage:
		var p SendMessagePayload
		if err := env.DecodePayload(&p); err != nil {
			return nil, fmt.Errorf("decode send_message: %w", err)
		}
		if *workspace == "" {
			return nil, fmt.Errorf("not registered")
		}
		if p.To == *workspace {
			return nil, fmt.Errorf("cannot send message to self")
		}
		msg := d.queue.Enqueue(*workspace, p.To, p.Message)
		d.history.Append(*workspace, p.To, p.Message)
		d.logger.Printf("message %s -> %s: %s", *workspace, p.To, truncate(p.Message, 50))

		// Try to push notification to target
		if entry, ok := d.registry.Get(p.To); ok {
			pushEnv, _ := NewEnvelope("", MsgPushMessage, &msg)
			entry.mu.Lock()
			d.writeEnvelope(entry.conn, pushEnv)
			entry.mu.Unlock()
		}

		return NewResponseEnvelope(env.ID, map[string]string{
			"message_id": msg.ID,
			"status":     "sent",
		})

	case MsgBroadcast:
		var p BroadcastPayload
		if err := env.DecodePayload(&p); err != nil {
			return nil, fmt.Errorf("decode broadcast: %w", err)
		}
		if *workspace == "" {
			return nil, fmt.Errorf("not registered")
		}
		workspaces := d.registry.List()
		var recipients []string
		for _, ws := range workspaces {
			if ws.Name == *workspace {
				continue
			}
			msg := d.queue.Enqueue(*workspace, ws.Name, p.Message)
			d.history.Append(*workspace, ws.Name, p.Message)
			recipients = append(recipients, ws.Name)

			// Push notification
			if entry, ok := d.registry.Get(ws.Name); ok {
				pushEnv, _ := NewEnvelope("", MsgPushMessage, &msg)
				entry.mu.Lock()
				d.writeEnvelope(entry.conn, pushEnv)
				entry.mu.Unlock()
			}
		}
		return NewResponseEnvelope(env.ID, map[string]interface{}{
			"recipients": recipients,
			"count":      len(recipients),
		})

	case MsgReadMessages:
		var p ReadMessagesPayload
		if err := env.DecodePayload(&p); err != nil {
			return nil, fmt.Errorf("decode read_messages: %w", err)
		}
		if *workspace == "" {
			return nil, fmt.Errorf("not registered")
		}
		limit := p.Limit
		if limit <= 0 {
			limit = 10
		}
		messages := d.queue.Dequeue(*workspace, limit, p.From)
		return NewResponseEnvelope(env.ID, &ReadMessagesResponse{Messages: messages})

	case MsgListWorkspaces:
		workspaces := d.registry.List()
		return NewResponseEnvelope(env.ID, &ListWorkspacesResponse{Workspaces: workspaces})

	case MsgSetStatus:
		var p SetStatusPayload
		if err := env.DecodePayload(&p); err != nil {
			return nil, fmt.Errorf("decode set_status: %w", err)
		}
		if *workspace == "" {
			return nil, fmt.Errorf("not registered")
		}
		d.registry.SetStatus(*workspace, p.Status)
		return NewResponseEnvelope(env.ID, map[string]string{"status": "updated"})

	case MsgSetShared:
		var p SetSharedPayload
		if err := env.DecodePayload(&p); err != nil {
			return nil, fmt.Errorf("decode set_shared: %w", err)
		}
		d.sharedMu.Lock()
		d.sharedValues[p.Key] = p.Value
		d.sharedMu.Unlock()
		return NewResponseEnvelope(env.ID, map[string]string{"status": "stored"})

	case MsgGetShared:
		var p GetSharedPayload
		if err := env.DecodePayload(&p); err != nil {
			return nil, fmt.Errorf("decode get_shared: %w", err)
		}
		d.sharedMu.RLock()
		val, found := d.sharedValues[p.Key]
		d.sharedMu.RUnlock()
		return NewResponseEnvelope(env.ID, &GetSharedResponse{Key: p.Key, Value: val, Found: found})

	case MsgListShared:
		d.sharedMu.RLock()
		vals := make(map[string]string, len(d.sharedValues))
		for k, v := range d.sharedValues {
			vals[k] = v
		}
		d.sharedMu.RUnlock()
		return NewResponseEnvelope(env.ID, &ListSharedResponse{Values: vals})

	default:
		return nil, fmt.Errorf("unknown message type: %s", env.Type)
	}
}

func (d *Daemon) writeEnvelope(conn net.Conn, env *Envelope) {
	data, err := json.Marshal(env)
	if err != nil {
		d.logger.Printf("marshal error: %v", err)
		return
	}
	data = append(data, '\n')
	conn.Write(data)
}

func truncate(s string, n int) string {
	if len(s) <= n {
		return s
	}
	return s[:n] + "..."
}
