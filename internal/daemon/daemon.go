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
	"regexp"
	"strings"
	"sync"
	"time"

	"github.com/ashon/ax/internal/types"
	"github.com/ashon/ax/internal/usage"
)

const DefaultSocketPath = "~/.local/state/ax/daemon.sock"

const duplicateSuppressionWindow = 15 * time.Second

var duplicateNoOpPattern = regexp.MustCompile(`(?i)\b(ack|acked|acknowledged|received|noted|thanks?|thank you|roger|copy that|working on it|on it|looking into it|in progress|still working|status|no update|no-op|noop|same update|same status)\b`)

func ExpandSocketPath(path string) string {
	if len(path) > 0 && path[0] == '~' {
		home, _ := os.UserHomeDir()
		path = filepath.Join(home, path[1:])
	}
	return path
}

type Daemon struct {
	socketPath    string
	registry      *Registry
	queue         *MessageQueue
	history       *History
	sharedValues  map[string]string
	sharedPath    string
	sharedMu      sync.RWMutex
	taskStore     *TaskStore
	wakeScheduler *WakeScheduler
	listener      net.Listener
	logger        *log.Logger
}

func New(socketPath string) *Daemon {
	sp := ExpandSocketPath(socketPath)
	stateDir := filepath.Dir(sp)
	logger := log.New(os.Stderr, "[ax-daemon] ", log.LstdFlags)
	queue := NewPersistentMessageQueue(stateDir)
	if err := queue.Load(); err != nil {
		logger.Printf("load queue state: %v", err)
	}
	history := NewHistory(stateDir, 500)
	if err := history.Load(); err != nil {
		logger.Printf("load history state: %v", err)
	}
	taskStore := NewTaskStore(stateDir)
	if err := taskStore.Load(); err != nil {
		logger.Printf("load task state: %v", err)
	}
	sharedPath := filepath.Join(stateDir, "shared_values.json")
	sharedValues, err := loadSharedValues(sharedPath)
	if err != nil {
		logger.Printf("load shared values: %v", err)
		sharedValues = make(map[string]string)
	}
	return &Daemon{
		socketPath:    sp,
		registry:      NewRegistry(),
		queue:         queue,
		history:       history,
		sharedValues:  sharedValues,
		sharedPath:    sharedPath,
		taskStore:     taskStore,
		wakeScheduler: NewWakeScheduler(queue, logger),
		logger:        logger,
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

	go d.wakeScheduler.Run(ctx)

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
		if d.registry.UnregisterIfConn(workspace, conn) {
			d.logger.Printf("workspace %q disconnected", workspace)
			d.refreshTaskSnapshots()
		} else {
			d.logger.Printf("workspace %q disconnected on stale connection; active registration preserved", workspace)
		}
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
		previousConn := d.registry.Register(p.Workspace, p.Dir, p.Description, conn)
		if previousConn != nil {
			d.logger.Printf("workspace %q re-registered; closing previous connection", p.Workspace)
			_ = previousConn.Close()
		}
		d.refreshTaskSnapshots()
		d.logger.Printf("registered workspace %q", p.Workspace)
		return NewResponseEnvelope(env.ID, map[string]string{"status": "registered"})

	case MsgUnregister:
		if *workspace != "" {
			if d.registry.UnregisterIfConn(*workspace, conn) {
				d.refreshTaskSnapshots()
				d.logger.Printf("unregistered workspace %q", *workspace)
			} else {
				d.logger.Printf("ignored unregister for workspace %q from stale connection", *workspace)
			}
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
		if d.shouldSuppressDuplicateMessage(*workspace, p.To, p.Message) {
			d.logger.Printf("suppressed duplicate no-op message %s -> %s: %s", *workspace, p.To, truncate(p.Message, 50))
			return NewResponseEnvelope(env.ID, map[string]string{
				"message_id": "",
				"status":     "suppressed",
			})
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

		// Schedule wake retry for the target workspace
		d.wakeScheduler.Schedule(p.To, *workspace)
		d.refreshTaskSnapshots()

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
			if d.shouldSuppressDuplicateMessage(*workspace, ws.Name, p.Message) {
				d.logger.Printf("suppressed duplicate no-op broadcast %s -> %s: %s", *workspace, ws.Name, truncate(p.Message, 50))
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
		d.refreshTaskSnapshots()
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
		// Cancel pending wake if no more messages remain
		if d.queue.PendingCount(*workspace) == 0 {
			d.wakeScheduler.Cancel(*workspace)
		}
		d.refreshTaskSnapshots()
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
		d.refreshTaskSnapshots()
		return NewResponseEnvelope(env.ID, map[string]string{"status": "updated"})

	case MsgSetShared:
		var p SetSharedPayload
		if err := env.DecodePayload(&p); err != nil {
			return nil, fmt.Errorf("decode set_shared: %w", err)
		}
		d.sharedMu.Lock()
		d.sharedValues[p.Key] = p.Value
		sharedValuesCopy := copySharedValues(d.sharedValues)
		d.sharedMu.Unlock()
		if err := persistSharedValues(d.sharedPath, sharedValuesCopy); err != nil {
			d.logger.Printf("persist shared values: %v", err)
		}
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

	case MsgUsageTrends:
		var p UsageTrendsPayload
		if err := env.DecodePayload(&p); err != nil {
			return nil, fmt.Errorf("decode usage_trends: %w", err)
		}
		since := time.Duration(p.SinceMinutes) * time.Minute
		bucket := time.Duration(p.BucketMinutes) * time.Minute
		now := time.Now()
		requests := make([]usage.WorkspaceBinding, 0, len(p.Workspaces))
		for _, req := range p.Workspaces {
			requests = append(requests, usage.WorkspaceBinding{
				Name: req.Workspace,
				Dir:  req.Cwd,
			})
		}
		trends, err := usage.QueryWorkspaceTrends(requests, now, since, bucket)
		if err != nil {
			return nil, fmt.Errorf("query usage_trends: %w", err)
		}
		return NewResponseEnvelope(env.ID, &UsageTrendsResponse{Trends: trends})

	case MsgCreateTask:
		var p CreateTaskPayload
		if err := env.DecodePayload(&p); err != nil {
			return nil, fmt.Errorf("decode create_task: %w", err)
		}
		if *workspace == "" {
			return nil, fmt.Errorf("not registered")
		}
		startMode := types.TaskStartMode(p.StartMode)
		switch startMode {
		case "", types.TaskStartDefault:
			startMode = types.TaskStartDefault
		case types.TaskStartFresh:
		default:
			return nil, fmt.Errorf("invalid task start mode %q", p.StartMode)
		}
		priority := types.TaskPriority(p.Priority)
		switch priority {
		case "", types.TaskPriorityNormal:
			priority = types.TaskPriorityNormal
		case types.TaskPriorityLow, types.TaskPriorityHigh, types.TaskPriorityUrgent:
		default:
			return nil, fmt.Errorf("invalid task priority %q", p.Priority)
		}
		task := d.taskStore.Create(p.Title, p.Description, p.Assignee, *workspace, startMode, priority, p.StaleAfterSeconds)
		d.refreshTaskSnapshots()
		task, _ = d.taskStore.Get(task.ID)
		d.logger.Printf("task created: %s (assignee=%s, by=%s)", task.ID, task.Assignee, *workspace)
		return NewResponseEnvelope(env.ID, &TaskResponse{Task: *task})

	case MsgUpdateTask:
		var p UpdateTaskPayload
		if err := env.DecodePayload(&p); err != nil {
			return nil, fmt.Errorf("decode update_task: %w", err)
		}
		if *workspace == "" {
			return nil, fmt.Errorf("not registered")
		}
		task, err := d.taskStore.Update(p.ID, p.Status, p.Result, p.Log, *workspace)
		if err != nil {
			return nil, err
		}
		d.refreshTaskSnapshots()
		task, _ = d.taskStore.Get(task.ID)
		d.logger.Printf("task updated: %s (status=%s)", task.ID, task.Status)
		return NewResponseEnvelope(env.ID, &TaskResponse{Task: *task})

	case MsgGetTask:
		var p GetTaskPayload
		if err := env.DecodePayload(&p); err != nil {
			return nil, fmt.Errorf("decode get_task: %w", err)
		}
		task, found := d.taskStore.Get(p.ID)
		if !found {
			return nil, fmt.Errorf("task %q not found", p.ID)
		}
		enriched := d.enrichTask(*task)
		return NewResponseEnvelope(env.ID, &TaskResponse{Task: enriched})

	case MsgListTasks:
		var p ListTasksPayload
		if err := env.DecodePayload(&p); err != nil {
			return nil, fmt.Errorf("decode list_tasks: %w", err)
		}
		tasks := d.taskStore.List(p.Assignee, p.CreatedBy, p.Status)
		for i := range tasks {
			tasks[i] = d.enrichTask(tasks[i])
		}
		return NewResponseEnvelope(env.ID, &ListTasksResponse{Tasks: tasks})

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

func (d *Daemon) refreshTaskSnapshots() {
	d.taskStore.Refresh(d.enrichTask)
}

func (d *Daemon) enrichTask(task types.Task) types.Task {
	task.StaleInfo = d.computeTaskStaleInfo(task)
	if task.Priority == "" {
		task.Priority = types.TaskPriorityNormal
	}
	return task
}

func (d *Daemon) computeTaskStaleInfo(task types.Task) *types.TaskStaleInfo {
	if task.Priority == "" {
		task.Priority = types.TaskPriorityNormal
	}
	if task.Status != types.TaskPending && task.Status != types.TaskInProgress {
		return nil
	}

	now := time.Now()
	lastProgressAt := task.UpdatedAt
	if len(task.Logs) > 0 {
		lastProgressAt = task.Logs[len(task.Logs)-1].Timestamp
	}
	age := now.Sub(lastProgressAt)
	if age < 0 {
		age = 0
	}

	pendingMessages := d.queue.Pending(task.Assignee)
	pendingCount := len(pendingMessages)
	lastMessageAt := d.lastRelevantMessageAt(task, pendingMessages)

	info := &types.TaskStaleInfo{
		LastProgressAt:  lastProgressAt,
		AgeSeconds:      int64(age / time.Second),
		PendingMessages: pendingCount,
		LastMessageAt:   lastMessageAt,
	}
	if d.wakeScheduler != nil {
		if wakeState, ok := d.wakeScheduler.State(task.Assignee); ok {
			info.WakePending = true
			info.WakeAttempts = wakeState.Attempts
			if !wakeState.NextRetry.IsZero() {
				nextRetry := wakeState.NextRetry
				info.NextWakeRetryAt = &nextRetry
			}
		}
	}

	if task.StaleAfterSeconds > 0 && age >= time.Duration(task.StaleAfterSeconds)*time.Second {
		info.IsStale = true
		info.Reason = fmt.Sprintf("no task progress update for %s (threshold %ds)", formatTaskAge(age), task.StaleAfterSeconds)
		info.RecommendedAction = "inspect the assignee workspace and either append a progress log or redispatch/recover the task"
	}

	switch {
	case task.Status == types.TaskPending && pendingCount == 0 && len(task.Logs) == 0:
		info.StateDivergence = true
		info.StateDivergenceNote = "task is still pending but no queued message remains for the assignee"
		if info.RecommendedAction == "" {
			info.RecommendedAction = "redispatch the task or confirm whether the assignee already consumed the request outside the task flow"
		}
	case task.Status == types.TaskInProgress && pendingCount > 0:
		info.StateDivergence = true
		info.StateDivergenceNote = fmt.Sprintf("task is in_progress while %d pending message(s) still exist for %s", pendingCount, task.Assignee)
		if info.RecommendedAction == "" {
			info.RecommendedAction = "check whether the pending inbox backlog or a missed handoff is preventing task completion"
		}
	}

	if info.Reason == "" && info.StateDivergence {
		info.Reason = info.StateDivergenceNote
	}
	if info.Reason == "" && pendingCount > 0 {
		info.Reason = fmt.Sprintf("%d pending message(s) queued for %s", pendingCount, task.Assignee)
	}
	if info.RecommendedAction == "" && info.WakePending {
		info.RecommendedAction = "wait for the scheduled wake retry or inspect the assignee workspace if retries keep failing"
	}
	if info.Reason == "" {
		info.Reason = "awaiting next progress update"
	}
	return info
}

func (d *Daemon) lastRelevantMessageAt(task types.Task, pending []types.Message) *time.Time {
	var latest *time.Time
	setLatest := func(ts time.Time) {
		if latest == nil || ts.After(*latest) {
			copyTs := ts
			latest = &copyTs
		}
	}

	for _, msg := range pending {
		setLatest(msg.CreatedAt)
	}
	for _, entry := range d.history.Recent(200) {
		if taskRelatedHistory(entry, task) {
			setLatest(entry.Timestamp)
		}
	}
	return latest
}

func taskRelatedHistory(entry HistoryEntry, task types.Task) bool {
	if strings.Contains(entry.Content, task.ID) {
		return true
	}
	if entry.From == task.CreatedBy && entry.To == task.Assignee {
		return true
	}
	if entry.From == task.Assignee && entry.To == task.CreatedBy {
		return true
	}
	return false
}

func formatTaskAge(d time.Duration) string {
	switch {
	case d < time.Minute:
		return fmt.Sprintf("%ds", int(d.Seconds()))
	case d < time.Hour:
		return fmt.Sprintf("%dm", int(d.Minutes()))
	case d < 24*time.Hour:
		return fmt.Sprintf("%dh", int(d.Hours()))
	default:
		return fmt.Sprintf("%dd", int(d.Hours()/24))
	}
}

func (d *Daemon) shouldSuppressDuplicateMessage(from, to, content string) bool {
	normalized := normalizeMessageForSuppression(content)
	if normalized == "" {
		return false
	}

	recent := d.history.RecentMatching(6, func(entry HistoryEntry) bool {
		if entry.From != from || entry.To != to {
			return false
		}
		return time.Since(entry.Timestamp) <= duplicateSuppressionWindow
	})
	if len(recent) == 0 {
		return false
	}

	for _, entry := range recent {
		if normalizeMessageForSuppression(entry.Content) == normalized {
			return true
		}
	}

	if !looksLikeNoOpStatusMessage(normalized) {
		return false
	}

	return false
}

func normalizeMessageForSuppression(content string) string {
	trimmed := strings.TrimSpace(strings.ToLower(content))
	if trimmed == "" {
		return ""
	}
	trimmed = strings.Join(strings.Fields(trimmed), " ")
	return trimmed
}

func looksLikeNoOpStatusMessage(normalized string) bool {
	if len(normalized) > 160 {
		return false
	}
	if strings.Contains(normalized, "task id:") {
		return false
	}
	if strings.Contains(normalized, "\n") {
		return false
	}
	return duplicateNoOpPattern.MatchString(normalized)
}

func loadSharedValues(path string) (map[string]string, error) {
	if path == "" {
		return make(map[string]string), nil
	}
	data, err := os.ReadFile(path)
	if err != nil {
		if os.IsNotExist(err) {
			return make(map[string]string), nil
		}
		return nil, err
	}
	if len(data) == 0 {
		return make(map[string]string), nil
	}
	var values map[string]string
	if err := json.Unmarshal(data, &values); err != nil {
		return nil, err
	}
	if values == nil {
		values = make(map[string]string)
	}
	return values, nil
}

func persistSharedValues(path string, values map[string]string) error {
	if path == "" {
		return nil
	}
	data, err := json.Marshal(values)
	if err != nil {
		return err
	}
	return os.WriteFile(path, data, 0o644)
}

func copySharedValues(values map[string]string) map[string]string {
	copied := make(map[string]string, len(values))
	for k, v := range values {
		copied[k] = v
	}
	return copied
}
