package daemon_test

import (
	"bufio"
	"context"
	"encoding/json"
	"net"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/ashon/amux/internal/daemon"
)

func startTestDaemon(t *testing.T) (string, context.CancelFunc) {
	t.Helper()
	dir := t.TempDir()
	socketPath := filepath.Join(dir, "test.sock")

	ctx, cancel := context.WithCancel(context.Background())
	d := daemon.New(socketPath)

	go func() {
		if err := d.Run(ctx); err != nil {
			t.Logf("daemon exited: %v", err)
		}
	}()

	// Wait for socket to be ready
	for i := 0; i < 50; i++ {
		if _, err := os.Stat(socketPath); err == nil {
			break
		}
		time.Sleep(10 * time.Millisecond)
	}

	return socketPath, cancel
}

func connectAndRegister(t *testing.T, socketPath, workspace string) (net.Conn, *bufio.Scanner) {
	t.Helper()
	conn, err := net.Dial("unix", socketPath)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}

	scanner := bufio.NewScanner(conn)
	scanner.Buffer(make([]byte, 1024*1024), 1024*1024)

	// Register
	env, _ := daemon.NewEnvelope("reg-1", daemon.MsgRegister, &daemon.RegisterPayload{
		Workspace: workspace,
	})
	data, _ := json.Marshal(env)
	conn.Write(append(data, '\n'))

	// Read response
	if !scanner.Scan() {
		t.Fatal("no register response")
	}

	return conn, scanner
}

func sendAndRead(t *testing.T, conn net.Conn, scanner *bufio.Scanner, env *daemon.Envelope) *daemon.Envelope {
	t.Helper()
	data, _ := json.Marshal(env)
	conn.Write(append(data, '\n'))

	if !scanner.Scan() {
		t.Fatal("no response")
	}

	var resp daemon.Envelope
	if err := json.Unmarshal(scanner.Bytes(), &resp); err != nil {
		t.Fatalf("unmarshal response: %v", err)
	}
	return &resp
}

func TestDaemonRegisterAndList(t *testing.T) {
	socketPath, cancel := startTestDaemon(t)
	defer cancel()

	conn1, scanner1 := connectAndRegister(t, socketPath, "backend")
	defer conn1.Close()

	conn2, scanner2 := connectAndRegister(t, socketPath, "frontend")
	defer conn2.Close()
	_ = scanner2

	// List workspaces from backend
	env, _ := daemon.NewEnvelope("list-1", daemon.MsgListWorkspaces, struct{}{})
	resp := sendAndRead(t, conn1, scanner1, env)

	if resp.Type != daemon.MsgResponse {
		t.Fatalf("expected response, got %s", resp.Type)
	}

	var respPayload daemon.ResponsePayload
	resp.DecodePayload(&respPayload)

	var listResp daemon.ListWorkspacesResponse
	json.Unmarshal(respPayload.Data, &listResp)

	if len(listResp.Workspaces) != 2 {
		t.Fatalf("expected 2 workspaces, got %d", len(listResp.Workspaces))
	}
	t.Logf("workspaces: %+v", listResp.Workspaces)
}

func TestDaemonSendAndReadMessage(t *testing.T) {
	socketPath, cancel := startTestDaemon(t)
	defer cancel()

	conn1, scanner1 := connectAndRegister(t, socketPath, "backend")
	defer conn1.Close()

	conn2, scanner2 := connectAndRegister(t, socketPath, "frontend")
	defer conn2.Close()

	// Backend sends message to frontend
	sendEnv, _ := daemon.NewEnvelope("send-1", daemon.MsgSendMessage, &daemon.SendMessagePayload{
		To:      "frontend",
		Message: "API /api/users is ready",
	})
	resp := sendAndRead(t, conn1, scanner1, sendEnv)

	if resp.Type != daemon.MsgResponse {
		t.Fatalf("expected response, got %s", resp.Type)
	}

	// Frontend should receive a push notification
	if !scanner2.Scan() {
		t.Fatal("no push message to frontend")
	}
	var pushEnv daemon.Envelope
	json.Unmarshal(scanner2.Bytes(), &pushEnv)
	if pushEnv.Type != daemon.MsgPushMessage {
		t.Fatalf("expected push_message, got %s", pushEnv.Type)
	}
	t.Logf("push received: %s", string(pushEnv.Payload))

	// Frontend reads messages
	readEnv, _ := daemon.NewEnvelope("read-1", daemon.MsgReadMessages, &daemon.ReadMessagesPayload{
		Limit: 10,
	})
	resp = sendAndRead(t, conn2, scanner2, readEnv)

	var respPayload daemon.ResponsePayload
	resp.DecodePayload(&respPayload)

	var readResp daemon.ReadMessagesResponse
	json.Unmarshal(respPayload.Data, &readResp)

	if len(readResp.Messages) != 1 {
		t.Fatalf("expected 1 message, got %d", len(readResp.Messages))
	}

	msg := readResp.Messages[0]
	if msg.From != "backend" || msg.Content != "API /api/users is ready" {
		t.Fatalf("unexpected message: %+v", msg)
	}
	t.Logf("message received: from=%s content=%s", msg.From, msg.Content)
}

func TestDaemonSharedValues(t *testing.T) {
	socketPath, cancel := startTestDaemon(t)
	defer cancel()

	conn1, scanner1 := connectAndRegister(t, socketPath, "backend")
	defer conn1.Close()

	conn2, scanner2 := connectAndRegister(t, socketPath, "frontend")
	defer conn2.Close()

	// Backend sets shared value
	setEnv, _ := daemon.NewEnvelope("set-1", daemon.MsgSetShared, &daemon.SetSharedPayload{
		Key:   "api_url",
		Value: "http://localhost:8080",
	})
	sendAndRead(t, conn1, scanner1, setEnv)

	// Frontend gets shared value
	getEnv, _ := daemon.NewEnvelope("get-1", daemon.MsgGetShared, &daemon.GetSharedPayload{
		Key: "api_url",
	})
	resp := sendAndRead(t, conn2, scanner2, getEnv)

	var respPayload daemon.ResponsePayload
	resp.DecodePayload(&respPayload)

	var getResp daemon.GetSharedResponse
	json.Unmarshal(respPayload.Data, &getResp)

	if !getResp.Found || getResp.Value != "http://localhost:8080" {
		t.Fatalf("expected api_url=http://localhost:8080, got found=%v value=%s", getResp.Found, getResp.Value)
	}
	t.Logf("shared value: %s = %s", getResp.Key, getResp.Value)
}
