package daemon

import (
	"fmt"
	"net"
	"strings"
	"time"

	"github.com/ashon/ax/internal/tmux"
	"github.com/ashon/ax/internal/types"
	"github.com/ashon/ax/internal/usage"
)

func requireRegisteredWorkspace(workspace string) error {
	if workspace == "" {
		return fmt.Errorf("not registered")
	}
	return nil
}

func (d *Daemon) sendPushEnvelope(target string, msg types.Message, droppedLog string) {
	if entry, ok := d.registry.Get(target); ok {
		pushEnv, _ := NewEnvelope("", MsgPushMessage, &msg)
		if !entry.Send(pushEnv, 100*time.Millisecond) {
			d.logger.Printf(droppedLog, target)
		}
	}
}

func (d *Daemon) handleRegisterEnvelope(conn net.Conn, env *Envelope, workspace *string) (*Envelope, error) {
	var p RegisterPayload
	if err := env.DecodePayload(&p); err != nil {
		return nil, fmt.Errorf("decode register: %w", err)
	}

	*workspace = p.Workspace
	entry, previous := d.registry.Register(p.Workspace, p.Dir, p.Description, p.ConfigPath, time.Duration(p.IdleTimeout)*time.Second, conn)
	d.startConnWriter(entry)
	if previous != nil {
		d.logger.Printf("workspace %q re-registered; closing previous connection", p.Workspace)
		previous.Close()
		_ = previous.Conn().Close()
	}
	rehydrated := d.rehydrateRunnableTaskMessages(p.Workspace, false, true)
	d.refreshTaskSnapshots()
	d.logger.Printf("registered workspace %q (rehydrated_tasks=%d)", p.Workspace, rehydrated)
	return NewResponseEnvelope(env.ID, &StatusResponse{Status: "registered"})
}

func (d *Daemon) handleUnregisterEnvelope(conn net.Conn, env *Envelope, workspace *string) (*Envelope, error) {
	if *workspace != "" {
		if d.registry.UnregisterIfConn(*workspace, conn) {
			d.refreshTaskSnapshots()
			d.logger.Printf("unregistered workspace %q", *workspace)
		} else {
			d.logger.Printf("ignored unregister for workspace %q from stale connection", *workspace)
		}
		*workspace = ""
	}
	return NewResponseEnvelope(env.ID, &StatusResponse{Status: "unregistered"})
}

func (d *Daemon) handleSendMessageEnvelope(env *Envelope, workspace string) (*Envelope, error) {
	var p SendMessagePayload
	if err := env.DecodePayload(&p); err != nil {
		return nil, fmt.Errorf("decode send_message: %w", err)
	}
	if err := requireRegisteredWorkspace(workspace); err != nil {
		return nil, err
	}
	if p.To == workspace {
		return nil, fmt.Errorf("cannot send message to self")
	}
	if d.shouldSuppressDuplicateMessage(workspace, p.To, p.Message) {
		d.logger.Printf("suppressed duplicate no-op message %s -> %s: %s", workspace, p.To, truncate(p.Message, 50))
		return NewResponseEnvelope(env.ID, &SendMessageResponse{
			Status: "suppressed",
		})
	}

	msg := taskAwareMessage(workspace, p.To, p.Message)
	msg = d.queue.EnqueueMessage(msg)
	d.taskStore.RecordDispatch(msg.TaskID, msg.To, msg.CreatedAt)
	d.history.AppendMessage(msg)
	d.logger.Printf("message %s -> %s: %s", workspace, p.To, truncate(p.Message, 50))
	d.registry.Touch(workspace)
	if d.canDeliverMessage(p.To, msg) {
		d.sendPushEnvelope(p.To, msg, "push to %q dropped (outbox full or closed); wake scheduler will retry")
		d.wakeScheduler.Schedule(p.To, workspace)
	} else {
		d.logger.Printf("withheld fresh-task delivery %s -> %s until %q registers a session newer than task creation", workspace, p.To, p.To)
	}
	d.refreshTaskSnapshots()

	if strings.TrimSpace(p.ConfigPath) != "" {
		fresh := d.freshTaskStartForMessage(p.To, workspace, p.Message)
		if err := d.sessionMgr.ensureRunnable(p.ConfigPath, p.To, workspace, fresh); err != nil {
			return nil, fmt.Errorf("dispatch %s -> %s: %w", workspace, p.To, err)
		}
	}

	return NewResponseEnvelope(env.ID, &SendMessageResponse{
		MessageID: msg.ID,
		Status:    "sent",
	})
}

func (d *Daemon) freshTaskStartForMessage(target, sender, message string) bool {
	taskID := extractTaskID(message)
	if taskID == "" {
		return false
	}
	task, ok := d.taskStore.Get(taskID)
	if !ok {
		return false
	}
	return task.Assignee == target && task.CreatedBy == sender && task.StartMode == types.TaskStartFresh
}

func (d *Daemon) handleBroadcastEnvelope(env *Envelope, workspace string) (*Envelope, error) {
	var p BroadcastPayload
	if err := env.DecodePayload(&p); err != nil {
		return nil, fmt.Errorf("decode broadcast: %w", err)
	}
	if err := requireRegisteredWorkspace(workspace); err != nil {
		return nil, err
	}

	workspaces := d.registry.List()
	var recipients []string
	for _, ws := range workspaces {
		if ws.Name == workspace {
			continue
		}
		if d.shouldSuppressDuplicateMessage(workspace, ws.Name, p.Message) {
			d.logger.Printf("suppressed duplicate no-op broadcast %s -> %s: %s", workspace, ws.Name, truncate(p.Message, 50))
			continue
		}

		msg := taskAwareMessage(workspace, ws.Name, p.Message)
		msg = d.queue.EnqueueMessage(msg)
		d.taskStore.RecordDispatch(msg.TaskID, msg.To, msg.CreatedAt)
		d.history.AppendMessage(msg)
		recipients = append(recipients, ws.Name)
		if d.canDeliverMessage(ws.Name, msg) {
			d.sendPushEnvelope(ws.Name, msg, "broadcast push to %q dropped (outbox full or closed)")
			d.wakeScheduler.Schedule(ws.Name, workspace)
		} else {
			d.logger.Printf("withheld fresh-task broadcast %s -> %s until %q registers a session newer than task creation", workspace, ws.Name, ws.Name)
		}
	}

	d.registry.Touch(workspace)
	d.refreshTaskSnapshots()

	if strings.TrimSpace(p.ConfigPath) != "" {
		for _, recipient := range recipients {
			if err := d.sessionMgr.ensureRunnable(p.ConfigPath, recipient, workspace, false); err != nil {
				return nil, fmt.Errorf("broadcast dispatch %s -> %s: %w", workspace, recipient, err)
			}
		}
	}

	return NewResponseEnvelope(env.ID, &BroadcastResponse{
		Recipients: recipients,
		Count:      len(recipients),
	})
}

func (d *Daemon) handleReadMessagesEnvelope(env *Envelope, workspace string) (*Envelope, error) {
	var p ReadMessagesPayload
	if err := env.DecodePayload(&p); err != nil {
		return nil, fmt.Errorf("decode read_messages: %w", err)
	}
	if err := requireRegisteredWorkspace(workspace); err != nil {
		return nil, err
	}

	limit := p.Limit
	if limit <= 0 {
		limit = 10
	}
	messages := d.queue.DequeueIf(workspace, limit, p.From, func(msg types.Message) bool {
		return d.canDeliverMessage(workspace, msg)
	})
	if len(messages) > 0 {
		d.registry.Touch(workspace)
	}
	if d.queue.PendingCountIf(workspace, func(msg types.Message) bool {
		return d.canDeliverMessage(workspace, msg)
	}) == 0 {
		if sender, ok := d.taskClaimFollowUpSender(workspace, messages); ok {
			d.wakeScheduler.Schedule(workspace, sender)
		} else {
			d.wakeScheduler.Cancel(workspace)
		}
	}
	d.refreshTaskSnapshots()
	return NewResponseEnvelope(env.ID, &ReadMessagesResponse{Messages: messages})
}

func (d *Daemon) handleListWorkspacesEnvelope(env *Envelope) (*Envelope, error) {
	workspaces := d.registry.List()
	return NewResponseEnvelope(env.ID, &ListWorkspacesResponse{Workspaces: workspaces})
}

func (d *Daemon) handleSetStatusEnvelope(env *Envelope, workspace string) (*Envelope, error) {
	var p SetStatusPayload
	if err := env.DecodePayload(&p); err != nil {
		return nil, fmt.Errorf("decode set_status: %w", err)
	}
	if err := requireRegisteredWorkspace(workspace); err != nil {
		return nil, err
	}

	d.registry.SetStatus(workspace, p.Status)
	d.registry.Touch(workspace)
	d.refreshTaskSnapshots()
	return NewResponseEnvelope(env.ID, &StatusResponse{Status: "updated"})
}

func (d *Daemon) handleSetSharedEnvelope(env *Envelope) (*Envelope, error) {
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
	return NewResponseEnvelope(env.ID, &StatusResponse{Status: "stored"})
}

func (d *Daemon) handleGetSharedEnvelope(env *Envelope) (*Envelope, error) {
	var p GetSharedPayload
	if err := env.DecodePayload(&p); err != nil {
		return nil, fmt.Errorf("decode get_shared: %w", err)
	}

	d.sharedMu.RLock()
	val, found := d.sharedValues[p.Key]
	d.sharedMu.RUnlock()
	return NewResponseEnvelope(env.ID, &GetSharedResponse{Key: p.Key, Value: val, Found: found})
}

func (d *Daemon) handleListSharedEnvelope(env *Envelope) (*Envelope, error) {
	d.sharedMu.RLock()
	vals := make(map[string]string, len(d.sharedValues))
	for k, v := range d.sharedValues {
		vals[k] = v
	}
	d.sharedMu.RUnlock()
	return NewResponseEnvelope(env.ID, &ListSharedResponse{Values: vals})
}

func (d *Daemon) handleUsageTrendsEnvelope(env *Envelope) (*Envelope, error) {
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
}

func (d *Daemon) handleCreateTaskEnvelope(env *Envelope, workspace string) (*Envelope, error) {
	var p CreateTaskPayload
	if err := env.DecodePayload(&p); err != nil {
		return nil, fmt.Errorf("decode create_task: %w", err)
	}
	if err := requireRegisteredWorkspace(workspace); err != nil {
		return nil, err
	}

	startMode, workflowMode, priority, err := parseTaskLifecycleOptions(p.StartMode, p.WorkflowMode, p.Priority)
	if err != nil {
		return nil, err
	}
	task, err := d.taskStore.CreateWithWorkflow(p.Title, p.Description, p.Assignee, workspace, p.ParentTaskID, startMode, workflowMode, priority, p.StaleAfterSeconds, "", d.dispatchConfigPathForWorkspace(workspace))
	if err != nil {
		return nil, err
	}
	d.registry.Touch(workspace)
	d.refreshTaskSnapshots()
	task, _ = d.enrichedTaskByID(task.ID)
	d.logger.Printf("task created: %s (assignee=%s, by=%s)", task.ID, task.Assignee, workspace)
	return NewResponseEnvelope(env.ID, &TaskResponse{Task: *task})
}

func (d *Daemon) handleStartTaskEnvelope(env *Envelope, workspace string) (*Envelope, error) {
	var p StartTaskPayload
	if err := env.DecodePayload(&p); err != nil {
		return nil, fmt.Errorf("decode start_task: %w", err)
	}
	if err := requireRegisteredWorkspace(workspace); err != nil {
		return nil, err
	}

	startMode, workflowMode, priority, err := parseTaskLifecycleOptions(p.StartMode, p.WorkflowMode, p.Priority)
	if err != nil {
		return nil, err
	}
	dispatchBody, err := normalizeTaskDispatchBody(p.Message)
	if err != nil {
		return nil, err
	}

	task, err := d.taskStore.CreateWithWorkflow(p.Title, p.Description, p.Assignee, workspace, p.ParentTaskID, startMode, workflowMode, priority, p.StaleAfterSeconds, dispatchBody, d.dispatchConfigPathForWorkspace(workspace))
	if err != nil {
		return nil, err
	}

	dispatch, err := d.dispatchTaskStart(*task)
	if err != nil {
		return nil, err
	}
	d.registry.Touch(workspace)
	d.refreshTaskSnapshots()
	task, _ = d.enrichedTaskByID(task.ID)
	d.logger.Printf("task started: %s (assignee=%s, by=%s, dispatch=%s)", task.ID, task.Assignee, workspace, dispatch.Status)
	return NewResponseEnvelope(env.ID, &StartTaskResponse{
		Task:     *task,
		Dispatch: dispatch,
	})
}

func (d *Daemon) handleUpdateTaskEnvelope(env *Envelope, workspace string) (*Envelope, error) {
	var p UpdateTaskPayload
	if err := env.DecodePayload(&p); err != nil {
		return nil, fmt.Errorf("decode update_task: %w", err)
	}
	if err := requireRegisteredWorkspace(workspace); err != nil {
		return nil, err
	}

	task, err := d.taskStore.Update(p.ID, p.Status, p.Result, p.Log, workspace)
	if err != nil {
		return nil, err
	}
	d.registry.Touch(workspace)
	d.releaseSerialWorkflowSuccessor(*task)
	d.refreshTaskSnapshots()
	task, _ = d.enrichedTaskByID(task.ID)
	d.logger.Printf("task updated: %s (status=%s)", task.ID, task.Status)
	return NewResponseEnvelope(env.ID, &TaskResponse{Task: *task})
}

func (d *Daemon) handleGetTaskEnvelope(env *Envelope) (*Envelope, error) {
	var p GetTaskPayload
	if err := env.DecodePayload(&p); err != nil {
		return nil, fmt.Errorf("decode get_task: %w", err)
	}

	task, found := d.enrichedTaskByID(p.ID)
	if !found {
		return nil, fmt.Errorf("task %q not found", p.ID)
	}
	return NewResponseEnvelope(env.ID, &TaskResponse{Task: *task})
}

func (d *Daemon) handleListTasksEnvelope(env *Envelope) (*Envelope, error) {
	var p ListTasksPayload
	if err := env.DecodePayload(&p); err != nil {
		return nil, fmt.Errorf("decode list_tasks: %w", err)
	}

	tasks := d.taskStore.List(p.Assignee, p.CreatedBy, p.Status)
	snapshot := d.taskSnapshotsByID()
	for i := range tasks {
		tasks[i] = d.enrichTaskWithSnapshot(tasks[i], snapshot)
	}
	return NewResponseEnvelope(env.ID, &ListTasksResponse{Tasks: tasks})
}

func (d *Daemon) handleCancelTaskEnvelope(env *Envelope, workspace string) (*Envelope, error) {
	var p CancelTaskPayload
	if err := env.DecodePayload(&p); err != nil {
		return nil, fmt.Errorf("decode cancel_task: %w", err)
	}
	if err := requireRegisteredWorkspace(workspace); err != nil {
		return nil, err
	}

	task, err := d.taskStore.Cancel(p.ID, p.Reason, workspace, p.ExpectedVersion)
	if err != nil {
		return nil, err
	}
	d.registry.Touch(workspace)
	dropped := d.queue.RemoveTaskMessages(task.Assignee, task.ID)
	if d.queue.PendingCount(task.Assignee) == 0 {
		d.wakeScheduler.Cancel(task.Assignee)
	}
	d.releaseSerialWorkflowSuccessor(*task)
	d.refreshTaskSnapshots()
	task, _ = d.enrichedTaskByID(task.ID)
	d.logger.Printf("task cancelled: %s (dropped_messages=%d)", task.ID, dropped)
	return NewResponseEnvelope(env.ID, &TaskResponse{Task: *task})
}

func (d *Daemon) handleRemoveTaskEnvelope(env *Envelope, workspace string) (*Envelope, error) {
	var p RemoveTaskPayload
	if err := env.DecodePayload(&p); err != nil {
		return nil, fmt.Errorf("decode remove_task: %w", err)
	}
	if err := requireRegisteredWorkspace(workspace); err != nil {
		return nil, err
	}

	task, err := d.taskStore.Remove(p.ID, p.Reason, workspace, p.ExpectedVersion)
	if err != nil {
		return nil, err
	}
	d.registry.Touch(workspace)
	_ = d.queue.RemoveTaskMessages(task.Assignee, task.ID)
	if d.queue.PendingCount(task.Assignee) == 0 {
		d.wakeScheduler.Cancel(task.Assignee)
	}
	d.refreshTaskSnapshots()
	task, _ = d.enrichedTaskByID(task.ID)
	d.logger.Printf("task removed: %s", task.ID)
	return NewResponseEnvelope(env.ID, &TaskResponse{Task: *task})
}

func (d *Daemon) handleInterveneTaskEnvelope(env *Envelope, workspace string) (*Envelope, error) {
	var p InterveneTaskPayload
	if err := env.DecodePayload(&p); err != nil {
		return nil, fmt.Errorf("decode intervene_task: %w", err)
	}
	if err := requireRegisteredWorkspace(workspace); err != nil {
		return nil, err
	}

	task, found := d.taskStore.Get(p.ID)
	if !found {
		return nil, fmt.Errorf("task %q not found", p.ID)
	}
	if err := validateTaskControl(task, workspace, p.ExpectedVersion, true); err != nil {
		return nil, err
	}
	if task.Status != types.TaskPending && task.Status != types.TaskInProgress && task.Status != types.TaskBlocked {
		return nil, fmt.Errorf("task %q is not pending/in_progress/blocked", task.ID)
	}

	resp := InterveneTaskResponse{
		Task:   *task,
		Action: p.Action,
		Status: "noop",
	}

	switch p.Action {
	case "wake":
		if strings.TrimSpace(task.DispatchConfigPath) != "" {
			if err := d.sessionMgr.ensureRunnable(task.DispatchConfigPath, task.Assignee, workspace, false); err != nil {
				return nil, fmt.Errorf("wake task %s: %w", task.ID, err)
			}
		} else {
			if !tmux.SessionExists(task.Assignee) {
				return nil, fmt.Errorf("workspace %q is not running", task.Assignee)
			}
			if err := tmux.WakeWorkspace(task.Assignee, WakePrompt(workspace, false)); err != nil {
				return nil, err
			}
		}
		resp.Status = "woken"
	case "interrupt":
		if !tmux.SessionExists(task.Assignee) {
			return nil, fmt.Errorf("workspace %q is not running", task.Assignee)
		}
		if err := tmux.InterruptWorkspace(task.Assignee); err != nil {
			return nil, err
		}
		resp.Status = "interrupted"
	case "retry":
		retried, err := d.taskStore.Retry(task.ID, p.Note, workspace, p.ExpectedVersion)
		if err != nil {
			return nil, err
		}
		_ = d.queue.RemoveTaskMessages(task.Assignee, task.ID)
		msg := taskAwareMessage(workspace, task.Assignee, buildTaskReminderMessage(*retried, strings.TrimSpace(p.Note)))
		msg = d.queue.EnqueueMessage(msg)
		d.taskStore.RecordDispatch(msg.TaskID, msg.To, msg.CreatedAt)
		d.history.AppendMessage(msg)
		if d.canDeliverMessage(task.Assignee, msg) {
			d.sendPushEnvelope(task.Assignee, msg, "intervention push to %q dropped (outbox full or closed)")
		}
		d.wakeScheduler.Schedule(task.Assignee, workspace)
		if strings.TrimSpace(retried.DispatchConfigPath) != "" {
			if err := d.sessionMgr.ensureRunnable(retried.DispatchConfigPath, task.Assignee, workspace, false); err != nil {
				return nil, fmt.Errorf("retry dispatch task %s: %w", task.ID, err)
			}
		}
		d.refreshTaskSnapshots()
		refreshed, _ := d.enrichedTaskByID(task.ID)
		resp.Task = *refreshed
		resp.Status = "queued"
		resp.MessageID = msg.ID
	default:
		return nil, fmt.Errorf("invalid intervene_task action %q", p.Action)
	}
	d.registry.Touch(workspace)

	return NewResponseEnvelope(env.ID, &resp)
}

func (d *Daemon) rehydrateRunnableTaskMessages(workspace string, push bool, scheduleWake bool) int {
	now := time.Now()
	runnable := d.taskStore.RunnableByAssignee(workspace, now)
	rehydrated := 0
	for _, task := range runnable {
		if d.queue.HasTaskMessage(workspace, task.ID) {
			continue
		}
		msg := taskAwareMessage(task.CreatedBy, task.Assignee, taskDispatchContent(task, ""))
		msg = d.queue.EnqueueMessage(msg)
		d.taskStore.RecordDispatch(msg.TaskID, msg.To, msg.CreatedAt)
		d.history.AppendMessage(msg)
		if push && d.canDeliverMessage(task.Assignee, msg) {
			d.sendPushEnvelope(task.Assignee, msg, "rehydrated task push to %q dropped (outbox full or closed)")
		}
		if scheduleWake {
			d.wakeScheduler.Schedule(task.Assignee, task.CreatedBy)
		}
		rehydrated++
	}
	return rehydrated
}

func (d *Daemon) recoverRunnableTaskMessages(workspace string) int {
	rehydrated := d.rehydrateRunnableTaskMessages(workspace, false, false)
	if rehydrated > 0 {
		d.refreshTaskSnapshots()
	}
	return rehydrated
}

func (d *Daemon) taskClaimFollowUpSender(workspace string, messages []types.Message) (string, bool) {
	for _, msg := range messages {
		taskID := messageTaskID(msg)
		if taskID == "" {
			continue
		}
		task, ok := d.taskStore.Get(taskID)
		if !ok || task.Assignee != workspace || task.Status != types.TaskPending || task.ClaimedAt != nil || task.LastDispatchAt == nil {
			continue
		}
		sender := strings.TrimSpace(msg.From)
		if sender == "" {
			sender = task.CreatedBy
		}
		if sender == "" {
			sender = workspace
		}
		return sender, true
	}
	return "", false
}

func taskDispatchContent(task types.Task, note string) string {
	if note == "" && strings.TrimSpace(task.DispatchMessage) != "" {
		return task.DispatchMessage
	}
	return buildTaskReminderMessage(task, note)
}

func (d *Daemon) handleGetTeamStateEnvelope(env *Envelope) (*Envelope, error) {
	var p GetTeamStatePayload
	if err := env.DecodePayload(&p); err != nil {
		return nil, fmt.Errorf("decode get_team_state: %w", err)
	}

	state, err := d.getTeamState(p.ConfigPath)
	if err != nil {
		return nil, err
	}
	return NewResponseEnvelope(env.ID, &TeamStateResponse{State: state})
}

func taskAwareMessage(from, to, content string) types.Message {
	return types.Message{
		From:    from,
		To:      to,
		Content: content,
		TaskID:  extractTaskID(content),
	}
}

func parseTaskLifecycleOptions(startModeValue, workflowModeValue, priorityValue string) (types.TaskStartMode, types.TaskWorkflowMode, types.TaskPriority, error) {
	startMode := types.TaskStartMode(strings.TrimSpace(startModeValue))
	switch startMode {
	case "", types.TaskStartDefault:
		startMode = types.TaskStartDefault
	case types.TaskStartFresh:
	default:
		return "", "", "", fmt.Errorf("invalid task start mode %q", startModeValue)
	}

	workflowMode := types.TaskWorkflowMode(strings.TrimSpace(workflowModeValue))
	switch workflowMode {
	case "", types.TaskWorkflowParallel:
		workflowMode = types.TaskWorkflowParallel
	case types.TaskWorkflowSerial:
	default:
		return "", "", "", fmt.Errorf("invalid task workflow mode %q", workflowModeValue)
	}

	priority := types.TaskPriority(strings.TrimSpace(priorityValue))
	switch priority {
	case "", types.TaskPriorityNormal:
		priority = types.TaskPriorityNormal
	case types.TaskPriorityLow, types.TaskPriorityHigh, types.TaskPriorityUrgent:
	default:
		return "", "", "", fmt.Errorf("invalid task priority %q", priorityValue)
	}

	return startMode, workflowMode, priority, nil
}

func normalizeTaskDispatchBody(message string) (string, error) {
	trimmed := strings.TrimSpace(message)
	if trimmed == "" {
		return "", fmt.Errorf("message is required")
	}
	if existingTaskID := extractTaskID(trimmed); existingTaskID != "" {
		return "", fmt.Errorf("message must not include Task ID %q; start_task injects the new task ID automatically", existingTaskID)
	}
	return trimmed, nil
}

func formatTaskDispatchMessage(taskID, message string) string {
	return fmt.Sprintf("Task ID: %s\n\n%s", taskID, strings.TrimSpace(message))
}

func (d *Daemon) dispatchTaskStart(task types.Task) (TaskDispatch, error) {
	snapshot := d.taskSnapshotsByID()
	task = d.enrichTaskWithSnapshot(task, snapshot)
	if strings.TrimSpace(task.DispatchMessage) == "" {
		return TaskDispatch{Status: "waiting_for_input"}, nil
	}
	if task.Sequence != nil && task.Sequence.State == types.TaskSequenceWaitingTurn {
		return TaskDispatch{Status: string(types.TaskSequenceWaitingTurn)}, nil
	}
	msg := taskAwareMessage(task.CreatedBy, task.Assignee, task.DispatchMessage)
	msg = d.queue.EnqueueMessage(msg)
	d.taskStore.RecordDispatch(msg.TaskID, msg.To, msg.CreatedAt)
	d.history.AppendMessage(msg)
	if d.canDeliverMessage(task.Assignee, msg) {
		d.sendPushEnvelope(task.Assignee, msg, "task start push to %q dropped (outbox full or closed)")
	}
	d.wakeScheduler.Schedule(task.Assignee, task.CreatedBy)
	dispatch := TaskDispatch{
		MessageID: msg.ID,
		Status:    "queued",
	}
	if strings.TrimSpace(task.DispatchConfigPath) == "" {
		return dispatch, nil
	}
	if err := d.sessionMgr.ensureRunnable(task.DispatchConfigPath, task.Assignee, task.CreatedBy, task.StartMode == types.TaskStartFresh); err != nil {
		return dispatch, fmt.Errorf("dispatch task %s: %w", task.ID, err)
	}
	return dispatch, nil
}

func (d *Daemon) releaseSerialWorkflowSuccessor(task types.Task) {
	if !isTerminalTaskStatus(task.Status) || strings.TrimSpace(task.ParentTaskID) == "" {
		return
	}
	parent, ok := d.taskStore.Get(task.ParentTaskID)
	if !ok || parent.WorkflowMode != types.TaskWorkflowSerial {
		return
	}

	for _, childID := range parent.ChildTaskIDs {
		child, ok := d.taskStore.Get(childID)
		if !ok || child.RemovedAt != nil || child.LastDispatchAt != nil || child.ClaimedAt != nil || strings.TrimSpace(child.DispatchMessage) == "" {
			continue
		}
		snapshot := d.taskSnapshotsByID()
		enriched := d.enrichTaskWithSnapshot(*child, snapshot)
		if enriched.Sequence == nil || enriched.Sequence.State != types.TaskSequenceReady {
			continue
		}
		if _, err := d.dispatchTaskStart(enriched); err != nil && d.logger != nil {
			d.logger.Printf("release serial successor %q failed: %v", enriched.ID, err)
		}
		return
	}
}

func (d *Daemon) dispatchConfigPathForWorkspace(name string) string {
	entry, ok := d.registry.Get(strings.TrimSpace(name))
	if !ok || entry == nil {
		return ""
	}
	return strings.TrimSpace(entry.configPath)
}

func buildTaskReminderMessage(task types.Task, note string) string {
	base := fmt.Sprintf("Task ID: %s\n\nTask: %s\nDescription: %s\nCurrent task status: %s\nThe daemon task registry still shows this task as runnable. Call get_task for the latest structured context, then continue or report a blocker.", task.ID, strings.TrimSpace(task.Title), strings.TrimSpace(task.Description), task.Status)
	if strings.TrimSpace(task.Description) == "" {
		base = fmt.Sprintf("Task ID: %s\n\nTask: %s\nCurrent task status: %s\nThe daemon task registry still shows this task as runnable. Call get_task for the latest structured context, then continue or report a blocker.", task.ID, strings.TrimSpace(task.Title), task.Status)
	}
	if note == "" {
		return base
	}
	return base + "\n\nOperator note: " + note
}

func (d *Daemon) handleDryRunTeamEnvelope(env *Envelope) (*Envelope, error) {
	var p TeamReconfigurePayload
	if err := env.DecodePayload(&p); err != nil {
		return nil, fmt.Errorf("decode dry_run_team_reconfigure: %w", err)
	}

	plan, err := d.dryRunTeamReconfigure(p.ConfigPath, p.ExpectedRevision, p.Changes)
	if err != nil {
		return nil, err
	}
	return NewResponseEnvelope(env.ID, &TeamPlanResponse{Plan: plan})
}

func (d *Daemon) handleApplyTeamEnvelope(env *Envelope) (*Envelope, error) {
	var p TeamReconfigurePayload
	if err := env.DecodePayload(&p); err != nil {
		return nil, fmt.Errorf("decode apply_team_reconfigure: %w", err)
	}

	ticket, err := d.beginTeamReconfigureApply(p.ConfigPath, p.ExpectedRevision, p.Changes, p.ReconcileMode)
	if err != nil {
		return nil, err
	}
	return NewResponseEnvelope(env.ID, &TeamApplyResponse{Ticket: ticket})
}

func (d *Daemon) handleFinishTeamEnvelope(env *Envelope) (*Envelope, error) {
	var p FinishTeamReconfigurePayload
	if err := env.DecodePayload(&p); err != nil {
		return nil, fmt.Errorf("decode finish_team_reconfigure: %w", err)
	}

	state, err := d.finishTeamReconfigureApply(p.Token, p.Success, p.Error, p.Actions)
	if err != nil {
		return nil, err
	}
	return NewResponseEnvelope(env.ID, &TeamStateResponse{State: state})
}
