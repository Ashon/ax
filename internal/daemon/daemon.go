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

	"github.com/ashon/ax/internal/daemonutil"
	"github.com/ashon/ax/internal/types"
)

const DefaultSocketPath = "~/.local/state/ax/daemon.sock"

const duplicateSuppressionWindow = 15 * time.Second

var duplicateNoOpPattern = regexp.MustCompile(`(?i)\b(ack|acked|acknowledged|received|noted|thanks?|thank you|roger|copy that|working on it|on it|looking into it|in progress|still working|status|no update|no-op|noop|same update|same status)\b`)

func ExpandSocketPath(path string) string {
	return daemonutil.ExpandSocketPath(path)
}

type Daemon struct {
	socketPath     string
	registry       *Registry
	queue          *MessageQueue
	history        *History
	sharedValues   map[string]string
	sharedPath     string
	sharedMu       sync.RWMutex
	taskStore      *TaskStore
	teamController *teamController
	wakeScheduler  *WakeScheduler
	listener       net.Listener
	logger         *log.Logger
}

func New(socketPath string) *Daemon {
	sp := ExpandSocketPath(socketPath)
	stateDir := filepath.Dir(sp)
	logger := log.New(os.Stderr, "[ax-daemon] ", log.LstdFlags)
	queue := NewPersistentMessageQueue(stateDir)
	queue.SetLogger(logger)
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
	teamStore := NewTeamStateStore(stateDir)
	if err := teamStore.Load(); err != nil {
		logger.Printf("load team state: %v", err)
	}
	sharedPath := filepath.Join(stateDir, "shared_values.json")
	sharedValues, err := loadSharedValues(sharedPath)
	if err != nil {
		logger.Printf("load shared values: %v", err)
		sharedValues = make(map[string]string)
	}
	d := &Daemon{
		socketPath:     sp,
		registry:       NewRegistry(),
		queue:          queue,
		history:        history,
		sharedValues:   sharedValues,
		sharedPath:     sharedPath,
		taskStore:      taskStore,
		teamController: newTeamController(stateDir, teamStore),
		wakeScheduler:  NewWakeScheduler(queue, logger),
		logger:         logger,
	}
	d.wakeScheduler.SetQueueRefiller(d.recoverRunnableTaskMessages)
	return d
}

func (d *Daemon) Run(ctx context.Context) error {
	defer d.queue.Close()

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
			d.dispatchResponse(conn, workspace, errEnv)
			continue
		}
		if resp != nil {
			d.dispatchResponse(conn, workspace, resp)
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

// dispatchResponse routes a synchronous response back to the originating
// connection. When the connection is registered, the response is queued on
// the connection's writer goroutine so it cannot interleave with concurrent
// push notifications. Otherwise we fall back to a direct, deadlined write.
func (d *Daemon) dispatchResponse(conn net.Conn, workspace string, env *Envelope) {
	if workspace != "" {
		if entry, ok := d.registry.Get(workspace); ok && entry.Conn() == conn {
			if !entry.Send(env, 5*time.Second) {
				d.logger.Printf("response to %q dropped (writer closed or busy)", workspace)
			}
			return
		}
	}
	if err := writeEnvelopeSync(conn, env); err != nil {
		d.logger.Printf("write response failed: %v", err)
	}
}

func (d *Daemon) handleEnvelope(conn net.Conn, env *Envelope, workspace *string) (*Envelope, error) {
	switch env.Type {
	case MsgRegister:
		return d.handleRegisterEnvelope(conn, env, workspace)

	case MsgUnregister:
		return d.handleUnregisterEnvelope(conn, env, workspace)

	case MsgSendMessage:
		return d.handleSendMessageEnvelope(env, *workspace)

	case MsgBroadcast:
		return d.handleBroadcastEnvelope(env, *workspace)

	case MsgReadMessages:
		return d.handleReadMessagesEnvelope(env, *workspace)

	case MsgListWorkspaces:
		return d.handleListWorkspacesEnvelope(env)

	case MsgSetStatus:
		return d.handleSetStatusEnvelope(env, *workspace)

	case MsgControlLifecycle:
		return d.handleControlLifecycleEnvelope(env, *workspace)

	case MsgSetShared:
		return d.handleSetSharedEnvelope(env)

	case MsgGetShared:
		return d.handleGetSharedEnvelope(env)

	case MsgListShared:
		return d.handleListSharedEnvelope(env)

	case MsgUsageTrends:
		return d.handleUsageTrendsEnvelope(env)

	case MsgCreateTask:
		return d.handleCreateTaskEnvelope(env, *workspace)

	case MsgStartTask:
		return d.handleStartTaskEnvelope(env, *workspace)

	case MsgUpdateTask:
		return d.handleUpdateTaskEnvelope(env, *workspace)

	case MsgGetTask:
		return d.handleGetTaskEnvelope(env)

	case MsgListTasks:
		return d.handleListTasksEnvelope(env)

	case MsgCancelTask:
		return d.handleCancelTaskEnvelope(env, *workspace)

	case MsgRemoveTask:
		return d.handleRemoveTaskEnvelope(env, *workspace)

	case MsgInterveneTask:
		return d.handleInterveneTaskEnvelope(env, *workspace)

	case MsgGetTeamState:
		return d.handleGetTeamStateEnvelope(env)

	case MsgDryRunTeam:
		return d.handleDryRunTeamEnvelope(env)

	case MsgApplyTeam:
		return d.handleApplyTeamEnvelope(env)

	case MsgFinishTeam:
		return d.handleFinishTeamEnvelope(env)

	default:
		return nil, fmt.Errorf("unknown message type: %s", env.Type)
	}
}

// writeDeadline bounds how long a single Write to a connection may block
// before being treated as a failure. Slow receivers cannot stall the
// daemon for longer than this.
const writeDeadline = 5 * time.Second

// writeEnvelopeSync marshals and writes a single envelope to conn with a
// bounded deadline. It is the only place that touches the underlying
// socket for outbound traffic; both the per-connection writer goroutine
// and the early (pre-registration) response path go through it.
func writeEnvelopeSync(conn net.Conn, env *Envelope) error {
	data, err := json.Marshal(env)
	if err != nil {
		return fmt.Errorf("marshal envelope: %w", err)
	}
	data = append(data, '\n')
	if err := conn.SetWriteDeadline(time.Now().Add(writeDeadline)); err != nil {
		// Some net.Conn implementations don't support deadlines; fall
		// through and attempt the write anyway.
		_ = err
	}
	if _, err := conn.Write(data); err != nil {
		return fmt.Errorf("write envelope: %w", err)
	}
	return nil
}

// startConnWriter spawns a goroutine that owns all asynchronous writes
// for a single connection entry. The writer drains the entry's outbox
// until the entry is closed; on any write error it closes the underlying
// connection so handleConn observes the disconnect and cleans up.
func (d *Daemon) startConnWriter(entry *connEntry) {
	go func() {
		for {
			select {
			case <-entry.closeCh:
				return
			case env := <-entry.outbox:
				if err := writeEnvelopeSync(entry.conn, env); err != nil {
					d.logger.Printf("write to %q failed: %v", entry.info.Name, err)
					entry.Close()
					_ = entry.conn.Close()
					return
				}
			}
		}
	}()
}

func truncate(s string, n int) string {
	if len(s) <= n {
		return s
	}
	return s[:n] + "..."
}

func (d *Daemon) canDeliverMessage(workspace string, msg types.Message) bool {
	taskID := messageTaskID(msg)
	if taskID == "" {
		return true
	}
	task, ok := d.taskStore.Get(taskID)
	if !ok || task.Assignee != workspace {
		return true
	}
	return !d.freshTaskDeliveryHeld(*task)
}

func (d *Daemon) freshTaskDeliveryHeld(task types.Task) bool {
	if task.StartMode != types.TaskStartFresh || task.ClaimedAt != nil || task.LastDispatchAt == nil {
		return false
	}
	entry, ok := d.registry.Get(task.Assignee)
	if !ok {
		return true
	}
	info := entry.Info()
	return info.ConnectedAt == nil || !info.ConnectedAt.After(task.CreatedAt)
}

func (d *Daemon) taskRunnableReason(task types.Task, pendingCount int, now time.Time) string {
	if task.Status != types.TaskPending {
		return ""
	}
	if task.Sequence != nil && task.Sequence.State == types.TaskSequenceWaitingTurn {
		return ""
	}
	if task.LastDispatchAt == nil || task.ClaimedAt != nil {
		return ""
	}
	if task.NextRetryAt != nil && task.NextRetryAt.After(now) {
		return ""
	}
	if d.freshTaskDeliveryHeld(task) || pendingCount > 0 {
		return ""
	}
	return "task registry shows the task should still run, but no queued task message remains; daemon can synthesize a reminder from task metadata"
}

func countTaskMessages(messages []types.Message, taskID string) int {
	taskID = strings.TrimSpace(taskID)
	if taskID == "" {
		return 0
	}
	count := 0
	for _, msg := range messages {
		if messageTaskID(msg) == taskID {
			count++
		}
	}
	return count
}

func (d *Daemon) refreshTaskSnapshots() {
	d.taskStore.Refresh(d.enrichTaskWithSnapshot)
}

func (d *Daemon) taskSnapshotsByID() map[string]types.Task {
	tasks := d.taskStore.Snapshot()
	snapshot := make(map[string]types.Task, len(tasks))
	for _, task := range tasks {
		snapshot[task.ID] = task
	}
	return snapshot
}

func (d *Daemon) enrichTask(task types.Task) types.Task {
	return d.enrichTaskWithSnapshot(task, d.taskSnapshotsByID())
}

func (d *Daemon) enrichTaskWithSnapshot(task types.Task, snapshot map[string]types.Task) types.Task {
	if task.WorkflowMode == "" {
		task.WorkflowMode = types.TaskWorkflowParallel
	}
	task.Sequence = d.computeTaskSequenceInfo(task, snapshot)
	task.StaleInfo = d.computeTaskStaleInfo(task)
	if task.Priority == "" {
		task.Priority = types.TaskPriorityNormal
	}
	return task
}

func (d *Daemon) computeTaskSequenceInfo(task types.Task, snapshot map[string]types.Task) *types.TaskSequenceInfo {
	parentID := strings.TrimSpace(task.ParentTaskID)
	if parentID == "" {
		return nil
	}
	parent, ok := snapshot[parentID]
	if !ok {
		return nil
	}
	mode := parent.WorkflowMode
	if mode == "" {
		mode = types.TaskWorkflowParallel
	}
	if mode != types.TaskWorkflowSerial {
		return nil
	}

	position := 0
	for idx, childID := range parent.ChildTaskIDs {
		if childID == task.ID {
			position = idx + 1
			break
		}
	}
	if position == 0 {
		return nil
	}

	info := &types.TaskSequenceInfo{
		Mode:     mode,
		State:    types.TaskSequenceReady,
		Position: position,
	}
	if task.LastDispatchAt != nil || task.ClaimedAt != nil || task.Status == types.TaskInProgress || task.Status == types.TaskBlocked || isTerminalTaskStatus(task.Status) {
		info.State = types.TaskSequenceReleased
		return info
	}

	for _, childID := range parent.ChildTaskIDs[:position-1] {
		sibling, ok := snapshot[childID]
		if !ok || sibling.RemovedAt != nil {
			continue
		}
		if isTerminalTaskStatus(sibling.Status) {
			continue
		}
		info.State = types.TaskSequenceWaitingTurn
		info.WaitingOnTaskID = sibling.ID
		return info
	}

	return info
}

func (d *Daemon) computeTaskStaleInfo(task types.Task) *types.TaskStaleInfo {
	if task.Priority == "" {
		task.Priority = types.TaskPriorityNormal
	}
	if task.Status != types.TaskPending && task.Status != types.TaskInProgress && task.Status != types.TaskBlocked {
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
	taskPendingCount := countTaskMessages(pendingMessages, task.ID)
	lastMessageAt := d.lastRelevantMessageAt(task, pendingMessages)

	info := &types.TaskStaleInfo{
		LastProgressAt:  lastProgressAt,
		AgeSeconds:      int64(age / time.Second),
		PendingMessages: pendingCount,
		LastMessageAt:   lastMessageAt,
	}
	if task.Sequence != nil && task.Sequence.State == types.TaskSequenceWaitingTurn {
		info.ClaimState = string(types.TaskSequenceWaitingTurn)
		info.ClaimStateNote = fmt.Sprintf("serial workflow is holding dispatch until prior child task %s becomes terminal", task.Sequence.WaitingOnTaskID)
		info.Reason = info.ClaimStateNote
		info.RecommendedAction = "finish, fail, or cancel the earlier child task before expecting this task to dispatch"
	} else if task.ClaimedAt != nil {
		info.ClaimState = "claimed"
		info.ClaimStateNote = fmt.Sprintf("first task-flow action recorded by %s via %s", task.ClaimedBy, task.ClaimSource)
	} else if task.LastDispatchAt != nil {
		info.ClaimState = "awaiting_claim"
		if d.freshTaskDeliveryHeld(task) {
			info.ClaimStateNote = "fresh-context start pending; queued dispatch is held until the assignee registers a session newer than task creation"
		} else if taskPendingCount > 0 {
			info.ClaimStateNote = "task dispatch is queued; waiting for the assignee's first task-flow action"
		} else {
			info.ClaimStateNote = "task dispatch is no longer queued and no first task-flow action has been recorded"
		}
	} else {
		info.ClaimState = "undispatched"
		info.ClaimStateNote = "task has no recorded task-aware dispatch to the assignee yet"
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
	if runnableReason := d.taskRunnableReason(task, taskPendingCount, now); runnableReason != "" {
		info.Runnable = true
		info.RunnableReason = runnableReason
		info.RecoveryEligible = true
		if info.RecommendedAction == "" {
			info.RecommendedAction = "re-register or wake the assignee so the daemon can rehydrate the runnable task reminder"
		}
	}
	if task.Status == types.TaskPending && task.ClaimedAt == nil && d.freshTaskDeliveryHeld(task) && info.RecommendedAction == "" {
		info.RecommendedAction = "recreate or re-register the assignee workspace, then wake it so it can call read_messages"
	}

	switch {
	case task.Status == types.TaskBlocked:
		if !info.IsStale {
			info.Reason = "task is blocked awaiting a retry or new input"
		}
		info.RecoveryEligible = true
		if info.RecommendedAction == "" {
			info.RecommendedAction = "send follow-up input or use intervene_task(... action=\"retry\") when the blocker is cleared"
		}
	case task.Status == types.TaskPending && task.ClaimedAt == nil && task.LastDispatchAt != nil && taskPendingCount == 0:
		info.StateDivergence = true
		info.StateDivergenceNote = "task dispatch was consumed or removed, but the assignee still has not produced a first task-flow action"
		if info.RecommendedAction == "" {
			info.RecommendedAction = "recover, redispatch, or reroute the task unless the assignee promptly emits an in-task update"
		}
	case task.Status == types.TaskPending && task.ClaimedAt == nil && task.LastDispatchAt == nil:
		if task.Sequence != nil && task.Sequence.State == types.TaskSequenceWaitingTurn {
			break
		}
		if info.Reason == "" {
			info.Reason = "task is pending and undispatched"
		}
	case task.Status == types.TaskInProgress && taskPendingCount > 0:
		info.StateDivergence = true
		info.StateDivergenceNote = fmt.Sprintf("task is claimed/in_progress while %d pending follow-up message(s) still exist for %s", taskPendingCount, task.Assignee)
		if info.RecommendedAction == "" {
			info.RecommendedAction = "check whether the pending inbox backlog or a missed handoff is preventing task completion"
		}
		if info.IsStale {
			info.RecoveryEligible = true
		}
	}
	if task.Rollup != nil && task.Rollup.NeedsParentReconciliation {
		info.StateDivergence = true
		info.StateDivergenceNote = "all child tasks are terminal, but the parent task still needs reconciliation"
		info.RecommendedAction = "record the synthesized parent result or close the umbrella task"
		if !info.IsStale {
			info.Reason = info.StateDivergenceNote
		}
	}

	if info.Reason == "" && info.StateDivergence {
		info.Reason = info.StateDivergenceNote
	}
	if info.Reason == "" && info.ClaimStateNote != "" {
		info.Reason = info.ClaimStateNote
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
	if entry.TaskID == task.ID || strings.Contains(entry.Content, task.ID) {
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

	// Even when the new message isn't an exact duplicate, suppress no-op
	// status chatter (e.g. "ack", "on it", "still working") if the sender
	// already pinged the same recipient within the suppression window.
	return looksLikeNoOpStatusMessage(normalized)
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
	return writeFileAtomic(path, data, 0o600)
}

func copySharedValues(values map[string]string) map[string]string {
	copied := make(map[string]string, len(values))
	for k, v := range values {
		copied[k] = v
	}
	return copied
}
