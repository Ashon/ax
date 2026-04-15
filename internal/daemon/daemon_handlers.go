package daemon

import (
	"fmt"
	"net"
	"time"

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
	entry, previous := d.registry.Register(p.Workspace, p.Dir, p.Description, conn)
	d.startConnWriter(entry)
	if previous != nil {
		d.logger.Printf("workspace %q re-registered; closing previous connection", p.Workspace)
		previous.Close()
		_ = previous.Conn().Close()
	}
	d.refreshTaskSnapshots()
	d.logger.Printf("registered workspace %q", p.Workspace)
	return NewResponseEnvelope(env.ID, map[string]string{"status": "registered"})
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
	return NewResponseEnvelope(env.ID, map[string]string{"status": "unregistered"})
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
		return NewResponseEnvelope(env.ID, map[string]string{
			"message_id": "",
			"status":     "suppressed",
		})
	}

	msg := d.queue.Enqueue(workspace, p.To, p.Message)
	d.history.Append(workspace, p.To, p.Message)
	d.logger.Printf("message %s -> %s: %s", workspace, p.To, truncate(p.Message, 50))
	d.sendPushEnvelope(p.To, msg, "push to %q dropped (outbox full or closed); wake scheduler will retry")
	d.wakeScheduler.Schedule(p.To, workspace)
	d.refreshTaskSnapshots()

	return NewResponseEnvelope(env.ID, map[string]string{
		"message_id": msg.ID,
		"status":     "sent",
	})
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

		msg := d.queue.Enqueue(workspace, ws.Name, p.Message)
		d.history.Append(workspace, ws.Name, p.Message)
		recipients = append(recipients, ws.Name)
		d.sendPushEnvelope(ws.Name, msg, "broadcast push to %q dropped (outbox full or closed)")
		d.wakeScheduler.Schedule(ws.Name, workspace)
	}

	d.refreshTaskSnapshots()
	return NewResponseEnvelope(env.ID, map[string]interface{}{
		"recipients": recipients,
		"count":      len(recipients),
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
	messages := d.queue.Dequeue(workspace, limit, p.From)
	if d.queue.PendingCount(workspace) == 0 {
		d.wakeScheduler.Cancel(workspace)
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
	d.refreshTaskSnapshots()
	return NewResponseEnvelope(env.ID, map[string]string{"status": "updated"})
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
	return NewResponseEnvelope(env.ID, map[string]string{"status": "stored"})
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

	task := d.taskStore.Create(p.Title, p.Description, p.Assignee, workspace, startMode, priority, p.StaleAfterSeconds)
	d.refreshTaskSnapshots()
	task, _ = d.taskStore.Get(task.ID)
	d.logger.Printf("task created: %s (assignee=%s, by=%s)", task.ID, task.Assignee, workspace)
	return NewResponseEnvelope(env.ID, &TaskResponse{Task: *task})
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
	d.refreshTaskSnapshots()
	task, _ = d.taskStore.Get(task.ID)
	d.logger.Printf("task updated: %s (status=%s)", task.ID, task.Status)
	return NewResponseEnvelope(env.ID, &TaskResponse{Task: *task})
}

func (d *Daemon) handleGetTaskEnvelope(env *Envelope) (*Envelope, error) {
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
}

func (d *Daemon) handleListTasksEnvelope(env *Envelope) (*Envelope, error) {
	var p ListTasksPayload
	if err := env.DecodePayload(&p); err != nil {
		return nil, fmt.Errorf("decode list_tasks: %w", err)
	}

	tasks := d.taskStore.List(p.Assignee, p.CreatedBy, p.Status)
	for i := range tasks {
		tasks[i] = d.enrichTask(tasks[i])
	}
	return NewResponseEnvelope(env.ID, &ListTasksResponse{Tasks: tasks})
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
