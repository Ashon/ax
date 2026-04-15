package daemon_test

import (
	"bufio"
	"context"
	"encoding/json"
	"net"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/types"
)

func startTestDaemon(t *testing.T) (string, context.CancelFunc) {
	t.Helper()
	dir, err := os.MkdirTemp("", "axd-")
	if err != nil {
		t.Fatalf("mkdtemp: %v", err)
	}
	t.Cleanup(func() { _ = os.RemoveAll(dir) })
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

func startTestDaemonAt(t *testing.T, socketPath string) (context.CancelFunc, <-chan struct{}) {
	t.Helper()

	ctx, cancel := context.WithCancel(context.Background())
	d := daemon.New(socketPath)
	done := make(chan struct{})

	go func() {
		defer close(done)
		if err := d.Run(ctx); err != nil {
			t.Logf("daemon exited: %v", err)
		}
	}()

	for i := 0; i < 50; i++ {
		if _, err := os.Stat(socketPath); err == nil {
			break
		}
		time.Sleep(10 * time.Millisecond)
	}

	return cancel, done
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

func createTaskWithStartMode(t *testing.T, conn net.Conn, scanner *bufio.Scanner, assignee, startMode string) string {
	t.Helper()

	createTaskEnv, _ := daemon.NewEnvelope("create-task", daemon.MsgCreateTask, &daemon.CreateTaskPayload{
		Title:       "Fresh delivery test",
		Description: "exercise fresh-task dispatch semantics",
		Assignee:    assignee,
		StartMode:   startMode,
	})
	createTaskResp := sendAndRead(t, conn, scanner, createTaskEnv)
	if createTaskResp.Type != daemon.MsgResponse {
		t.Fatalf("expected create_task response, got %s", createTaskResp.Type)
	}

	var payload daemon.ResponsePayload
	if err := createTaskResp.DecodePayload(&payload); err != nil {
		t.Fatalf("decode create task payload: %v", err)
	}
	var taskResp daemon.TaskResponse
	if err := json.Unmarshal(payload.Data, &taskResp); err != nil {
		t.Fatalf("unmarshal create task response: %v", err)
	}
	return taskResp.Task.ID
}

func startTaskWithParent(t *testing.T, conn net.Conn, scanner *bufio.Scanner, title, assignee, message, parentTaskID, workflowMode string) daemon.StartTaskResponse {
	t.Helper()

	startTaskEnv, _ := daemon.NewEnvelope("start-task", daemon.MsgStartTask, &daemon.StartTaskPayload{
		Title:        title,
		Assignee:     assignee,
		Message:      message,
		ParentTaskID: parentTaskID,
		WorkflowMode: workflowMode,
	})
	startTaskResp := sendAndRead(t, conn, scanner, startTaskEnv)
	if startTaskResp.Type != daemon.MsgResponse {
		t.Fatalf("expected start_task response, got %s", startTaskResp.Type)
	}

	var payload daemon.ResponsePayload
	if err := startTaskResp.DecodePayload(&payload); err != nil {
		t.Fatalf("decode start task payload: %v", err)
	}
	var taskResp daemon.StartTaskResponse
	if err := json.Unmarshal(payload.Data, &taskResp); err != nil {
		t.Fatalf("unmarshal start task response: %v", err)
	}
	return taskResp
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

func TestDaemonRejectsSelfMessage(t *testing.T) {
	socketPath, cancel := startTestDaemon(t)
	defer cancel()

	conn, scanner := connectAndRegister(t, socketPath, "ops-monitoring")
	defer conn.Close()

	sendEnv, _ := daemon.NewEnvelope("send-self-1", daemon.MsgSendMessage, &daemon.SendMessagePayload{
		To:      "ops-monitoring",
		Message: "check your own queue",
	})
	resp := sendAndRead(t, conn, scanner, sendEnv)

	if resp.Type != daemon.MsgError {
		t.Fatalf("expected error, got %s", resp.Type)
	}

	var errPayload daemon.ErrorPayload
	if err := resp.DecodePayload(&errPayload); err != nil {
		t.Fatalf("decode error payload: %v", err)
	}
	if errPayload.Message != "cannot send message to self" {
		t.Fatalf("unexpected error message: %q", errPayload.Message)
	}

	readEnv, _ := daemon.NewEnvelope("read-self-1", daemon.MsgReadMessages, &daemon.ReadMessagesPayload{
		Limit: 10,
	})
	readResp := sendAndRead(t, conn, scanner, readEnv)

	if readResp.Type != daemon.MsgResponse {
		t.Fatalf("expected response, got %s", readResp.Type)
	}

	var respPayload daemon.ResponsePayload
	readResp.DecodePayload(&respPayload)

	var readMessagesResp daemon.ReadMessagesResponse
	json.Unmarshal(respPayload.Data, &readMessagesResp)
	if len(readMessagesResp.Messages) != 0 {
		t.Fatalf("expected no queued self messages, got %d", len(readMessagesResp.Messages))
	}
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

func TestDaemonSuppressesDuplicateNoOpMessagesWithinWindow(t *testing.T) {
	socketPath, cancel := startTestDaemon(t)
	defer cancel()

	conn1, scanner1 := connectAndRegister(t, socketPath, "orchestrator")
	defer conn1.Close()

	conn2, scanner2 := connectAndRegister(t, socketPath, "worker")
	defer conn2.Close()

	sendEnv, _ := daemon.NewEnvelope("send-dup-1", daemon.MsgSendMessage, &daemon.SendMessagePayload{
		To:      "worker",
		Message: "ack",
	})
	resp1 := sendAndRead(t, conn1, scanner1, sendEnv)
	if resp1.Type != daemon.MsgResponse {
		t.Fatalf("expected response, got %s", resp1.Type)
	}

	if !scanner2.Scan() {
		t.Fatal("expected first push notification")
	}

	sendEnv2, _ := daemon.NewEnvelope("send-dup-2", daemon.MsgSendMessage, &daemon.SendMessagePayload{
		To:      "worker",
		Message: " ack ",
	})
	resp2 := sendAndRead(t, conn1, scanner1, sendEnv2)
	if resp2.Type != daemon.MsgResponse {
		t.Fatalf("expected response, got %s", resp2.Type)
	}

	var payload daemon.ResponsePayload
	resp2.DecodePayload(&payload)
	var result map[string]string
	if err := json.Unmarshal(payload.Data, &result); err != nil {
		t.Fatalf("unmarshal suppressed response: %v", err)
	}
	if result["status"] != "suppressed" {
		t.Fatalf("expected suppressed status, got %#v", result)
	}

	readEnv, _ := daemon.NewEnvelope("read-dup", daemon.MsgReadMessages, &daemon.ReadMessagesPayload{Limit: 10})
	readResp := sendAndRead(t, conn2, scanner2, readEnv)
	var readPayload daemon.ResponsePayload
	readResp.DecodePayload(&readPayload)
	var readMessages daemon.ReadMessagesResponse
	if err := json.Unmarshal(readPayload.Data, &readMessages); err != nil {
		t.Fatalf("unmarshal read messages: %v", err)
	}
	if len(readMessages.Messages) != 1 {
		t.Fatalf("expected only one delivered message, got %d", len(readMessages.Messages))
	}
}

func TestDaemonSuppressesNoOpStatusChatterWithinWindow(t *testing.T) {
	socketPath, cancel := startTestDaemon(t)
	defer cancel()

	conn1, scanner1 := connectAndRegister(t, socketPath, "orchestrator")
	defer conn1.Close()

	conn2, scanner2 := connectAndRegister(t, socketPath, "worker")
	defer conn2.Close()

	// First "ack" goes through normally.
	firstEnv, _ := daemon.NewEnvelope("noop-1", daemon.MsgSendMessage, &daemon.SendMessagePayload{
		To:      "worker",
		Message: "ack",
	})
	resp1 := sendAndRead(t, conn1, scanner1, firstEnv)
	if resp1.Type != daemon.MsgResponse {
		t.Fatalf("expected first message to be accepted, got %s", resp1.Type)
	}
	if !scanner2.Scan() {
		t.Fatal("expected push notification for first message")
	}

	// Second message is a different no-op status phrase but should be
	// suppressed because there is already recent chatter from -> to.
	secondEnv, _ := daemon.NewEnvelope("noop-2", daemon.MsgSendMessage, &daemon.SendMessagePayload{
		To:      "worker",
		Message: "still working on it",
	})
	resp2 := sendAndRead(t, conn1, scanner1, secondEnv)
	if resp2.Type != daemon.MsgResponse {
		t.Fatalf("expected second response, got %s", resp2.Type)
	}

	var payload daemon.ResponsePayload
	resp2.DecodePayload(&payload)
	var result map[string]string
	if err := json.Unmarshal(payload.Data, &result); err != nil {
		t.Fatalf("unmarshal suppressed response: %v", err)
	}
	if result["status"] != "suppressed" {
		t.Fatalf("expected no-op chatter to be suppressed, got %#v", result)
	}

	readEnv, _ := daemon.NewEnvelope("read-noop", daemon.MsgReadMessages, &daemon.ReadMessagesPayload{Limit: 10})
	readResp := sendAndRead(t, conn2, scanner2, readEnv)
	var readPayload daemon.ResponsePayload
	readResp.DecodePayload(&readPayload)
	var readMessages daemon.ReadMessagesResponse
	if err := json.Unmarshal(readPayload.Data, &readMessages); err != nil {
		t.Fatalf("unmarshal read messages: %v", err)
	}
	if len(readMessages.Messages) != 1 {
		t.Fatalf("expected only the first message delivered, got %d", len(readMessages.Messages))
	}
	if readMessages.Messages[0].Content != "ack" {
		t.Fatalf("expected first ack delivered, got %q", readMessages.Messages[0].Content)
	}
}

func TestDaemonDoesNotSuppressMeaningfulFollowUp(t *testing.T) {
	socketPath, cancel := startTestDaemon(t)
	defer cancel()

	conn1, scanner1 := connectAndRegister(t, socketPath, "orchestrator")
	defer conn1.Close()

	conn2, scanner2 := connectAndRegister(t, socketPath, "worker")
	defer conn2.Close()

	firstEnv, _ := daemon.NewEnvelope("real-1", daemon.MsgSendMessage, &daemon.SendMessagePayload{
		To:      "worker",
		Message: "ack",
	})
	if resp := sendAndRead(t, conn1, scanner1, firstEnv); resp.Type != daemon.MsgResponse {
		t.Fatalf("expected first response, got %s", resp.Type)
	}
	if !scanner2.Scan() {
		t.Fatal("expected first push notification")
	}

	// A real instruction must NOT be suppressed even within the window.
	followEnv, _ := daemon.NewEnvelope("real-2", daemon.MsgSendMessage, &daemon.SendMessagePayload{
		To:      "worker",
		Message: "Please regenerate the API client from openapi.yaml and run the integration suite.",
	})
	resp := sendAndRead(t, conn1, scanner1, followEnv)
	if resp.Type != daemon.MsgResponse {
		t.Fatalf("expected response, got %s", resp.Type)
	}
	var payload daemon.ResponsePayload
	resp.DecodePayload(&payload)
	var result map[string]string
	_ = json.Unmarshal(payload.Data, &result)
	if result["status"] == "suppressed" {
		t.Fatalf("real follow-up instruction was incorrectly suppressed: %#v", result)
	}

	if !scanner2.Scan() {
		t.Fatal("expected second push notification for real instruction")
	}
}

func TestDaemonDoesNotSuppressTaskDispatchMessages(t *testing.T) {
	socketPath, cancel := startTestDaemon(t)
	defer cancel()

	conn1, scanner1 := connectAndRegister(t, socketPath, "orchestrator")
	defer conn1.Close()

	conn2, scanner2 := connectAndRegister(t, socketPath, "worker")
	defer conn2.Close()

	message1 := "Task ID: 11111111-1111-1111-1111-111111111111\nack"
	message2 := "Task ID: 22222222-2222-2222-2222-222222222222\nack"
	sendEnv1, _ := daemon.NewEnvelope("send-task-1", daemon.MsgSendMessage, &daemon.SendMessagePayload{
		To:      "worker",
		Message: message1,
	})
	sendEnv2, _ := daemon.NewEnvelope("send-task-2", daemon.MsgSendMessage, &daemon.SendMessagePayload{
		To:      "worker",
		Message: message2,
	})

	resp1 := sendAndRead(t, conn1, scanner1, sendEnv1)
	resp2 := sendAndRead(t, conn1, scanner1, sendEnv2)
	if resp1.Type != daemon.MsgResponse || resp2.Type != daemon.MsgResponse {
		t.Fatalf("expected normal responses, got %s and %s", resp1.Type, resp2.Type)
	}

	var payload1, payload2 daemon.ResponsePayload
	resp1.DecodePayload(&payload1)
	resp2.DecodePayload(&payload2)
	var result1, result2 map[string]string
	_ = json.Unmarshal(payload1.Data, &result1)
	_ = json.Unmarshal(payload2.Data, &result2)
	if result1["status"] == "suppressed" || result2["status"] == "suppressed" {
		t.Fatalf("task dispatch messages should not be suppressed: %#v %#v", result1, result2)
	}

	for i := 0; i < 2; i++ {
		if !scanner2.Scan() {
			t.Fatalf("expected push notification %d", i+1)
		}
	}
}

func TestDaemonFreshTaskDispatchWaitsForNewerWorkerRegistration(t *testing.T) {
	socketPath, cancel := startTestDaemon(t)
	defer cancel()

	connOrch, scannerOrch := connectAndRegister(t, socketPath, "orchestrator")
	defer connOrch.Close()

	connWorker, scannerWorker := connectAndRegister(t, socketPath, "worker")

	taskID := createTaskWithStartMode(t, connOrch, scannerOrch, "worker", "fresh")
	message := "Task ID: " + taskID + "\nInspect fresh start barrier"
	sendEnv, _ := daemon.NewEnvelope("send-fresh-held", daemon.MsgSendMessage, &daemon.SendMessagePayload{
		To:      "worker",
		Message: message,
	})
	sendResp := sendAndRead(t, connOrch, scannerOrch, sendEnv)
	if sendResp.Type != daemon.MsgResponse {
		t.Fatalf("expected send_message response, got %s", sendResp.Type)
	}

	readEnv, _ := daemon.NewEnvelope("read-fresh-held", daemon.MsgReadMessages, &daemon.ReadMessagesPayload{Limit: 10})
	readResp := sendAndRead(t, connWorker, scannerWorker, readEnv)
	if readResp.Type != daemon.MsgResponse {
		t.Fatalf("expected held fresh task to avoid pre-read push, got %s", readResp.Type)
	}
	var readPayload daemon.ResponsePayload
	if err := readResp.DecodePayload(&readPayload); err != nil {
		t.Fatalf("decode held read payload: %v", err)
	}
	var readMessages daemon.ReadMessagesResponse
	if err := json.Unmarshal(readPayload.Data, &readMessages); err != nil {
		t.Fatalf("unmarshal held read response: %v", err)
	}
	if len(readMessages.Messages) != 0 {
		t.Fatalf("expected fresh dispatch to stay queued until worker re-registers, got %+v", readMessages.Messages)
	}

	_ = connWorker.Close()

	connWorkerFresh, scannerWorkerFresh := connectAndRegister(t, socketPath, "worker")
	defer connWorkerFresh.Close()

	readFreshResp := sendAndRead(t, connWorkerFresh, scannerWorkerFresh, readEnv)
	if readFreshResp.Type != daemon.MsgResponse {
		t.Fatalf("expected read_messages response after fresh register, got %s", readFreshResp.Type)
	}
	if err := readFreshResp.DecodePayload(&readPayload); err != nil {
		t.Fatalf("decode fresh read payload: %v", err)
	}
	if err := json.Unmarshal(readPayload.Data, &readMessages); err != nil {
		t.Fatalf("unmarshal fresh read response: %v", err)
	}
	if len(readMessages.Messages) != 1 || readMessages.Messages[0].TaskID != taskID {
		t.Fatalf("expected queued fresh task after re-register, got %+v", readMessages.Messages)
	}
}

func TestDaemonStartTaskQueuesInitialDispatch(t *testing.T) {
	socketPath, cancel := startTestDaemon(t)
	defer cancel()

	connOrch, scannerOrch := connectAndRegister(t, socketPath, "orchestrator")
	defer connOrch.Close()

	connWorker, scannerWorker := connectAndRegister(t, socketPath, "worker")
	defer connWorker.Close()

	started := startTaskWithParent(t, connOrch, scannerOrch, "Daemon-backed start", "worker", "Inspect the queued daemon start path", "", "")
	if started.Task.ID == "" {
		t.Fatal("expected start_task to create a task ID")
	}
	if started.Dispatch.Status != "queued" {
		t.Fatalf("dispatch status = %q, want queued", started.Dispatch.Status)
	}
	if started.Dispatch.MessageID == "" {
		t.Fatal("expected queued dispatch message ID")
	}
	wantMessage := "Task ID: " + started.Task.ID + "\n\nInspect the queued daemon start path"
	if started.Task.DispatchMessage != wantMessage {
		t.Fatalf("dispatch message = %q, want %q", started.Task.DispatchMessage, wantMessage)
	}

	if !scannerWorker.Scan() {
		t.Fatal("expected push notification for start_task")
	}
	var pushEnv daemon.Envelope
	if err := json.Unmarshal(scannerWorker.Bytes(), &pushEnv); err != nil {
		t.Fatalf("unmarshal push: %v", err)
	}
	if pushEnv.Type != daemon.MsgPushMessage {
		t.Fatalf("expected push_message, got %s", pushEnv.Type)
	}

	readEnv, _ := daemon.NewEnvelope("read-start-task", daemon.MsgReadMessages, &daemon.ReadMessagesPayload{Limit: 10})
	readResp := sendAndRead(t, connWorker, scannerWorker, readEnv)
	if readResp.Type != daemon.MsgResponse {
		t.Fatalf("expected read_messages response, got %s", readResp.Type)
	}
	var readPayload daemon.ResponsePayload
	if err := readResp.DecodePayload(&readPayload); err != nil {
		t.Fatalf("decode read payload: %v", err)
	}
	var readMessages daemon.ReadMessagesResponse
	if err := json.Unmarshal(readPayload.Data, &readMessages); err != nil {
		t.Fatalf("unmarshal read response: %v", err)
	}
	if len(readMessages.Messages) != 1 {
		t.Fatalf("expected one queued start_task message, got %+v", readMessages.Messages)
	}
	if readMessages.Messages[0].TaskID != started.Task.ID || readMessages.Messages[0].Content != wantMessage {
		t.Fatalf("unexpected start_task delivery: %+v", readMessages.Messages[0])
	}
}

func TestDaemonSerialWorkflowReleasesNextChildAfterTerminalSibling(t *testing.T) {
	socketPath, cancel := startTestDaemon(t)
	defer cancel()

	connOrch, scannerOrch := connectAndRegister(t, socketPath, "orchestrator")
	defer connOrch.Close()

	connWorker, scannerWorker := connectAndRegister(t, socketPath, "worker")
	defer connWorker.Close()

	parentEnv, _ := daemon.NewEnvelope("create-parent", daemon.MsgCreateTask, &daemon.CreateTaskPayload{
		Title:        "Serial parent",
		Assignee:     "orchestrator",
		WorkflowMode: string(types.TaskWorkflowSerial),
	})
	parentResp := sendAndRead(t, connOrch, scannerOrch, parentEnv)
	if parentResp.Type != daemon.MsgResponse {
		t.Fatalf("expected create_task response, got %s", parentResp.Type)
	}
	var parentPayload daemon.ResponsePayload
	if err := parentResp.DecodePayload(&parentPayload); err != nil {
		t.Fatalf("decode parent payload: %v", err)
	}
	var parentTask daemon.TaskResponse
	if err := json.Unmarshal(parentPayload.Data, &parentTask); err != nil {
		t.Fatalf("unmarshal parent task: %v", err)
	}

	first := startTaskWithParent(t, connOrch, scannerOrch, "First child", "worker", "First serial child", parentTask.Task.ID, "")
	if first.Dispatch.Status != "queued" {
		t.Fatalf("first child dispatch status = %q, want queued", first.Dispatch.Status)
	}
	if !scannerWorker.Scan() {
		t.Fatal("expected first child push")
	}

	second := startTaskWithParent(t, connOrch, scannerOrch, "Second child", "worker", "Second serial child", parentTask.Task.ID, "")
	if second.Dispatch.Status != "waiting_turn" {
		t.Fatalf("second child dispatch status = %q, want waiting_turn", second.Dispatch.Status)
	}
	if second.Task.Sequence == nil || second.Task.Sequence.State != "waiting_turn" || second.Task.Sequence.WaitingOnTaskID != first.Task.ID {
		t.Fatalf("unexpected second child sequence info: %+v", second.Task.Sequence)
	}
	if second.Task.LastDispatchAt != nil {
		t.Fatalf("expected second child dispatch to stay deferred, got %+v", second.Task.LastDispatchAt)
	}

	readEnv, _ := daemon.NewEnvelope("read-serial-empty", daemon.MsgReadMessages, &daemon.ReadMessagesPayload{Limit: 10})
	readResp := sendAndRead(t, connWorker, scannerWorker, readEnv)
	if readResp.Type != daemon.MsgResponse {
		t.Fatalf("expected read_messages response, got %s", readResp.Type)
	}
	var readPayload daemon.ResponsePayload
	if err := readResp.DecodePayload(&readPayload); err != nil {
		t.Fatalf("decode empty read payload: %v", err)
	}
	var readMessages daemon.ReadMessagesResponse
	if err := json.Unmarshal(readPayload.Data, &readMessages); err != nil {
		t.Fatalf("unmarshal empty read response: %v", err)
	}
	if len(readMessages.Messages) != 1 || readMessages.Messages[0].TaskID != first.Task.ID {
		t.Fatalf("expected only first child to be queued before release, got %+v", readMessages.Messages)
	}

	cancelEnv, _ := daemon.NewEnvelope("cancel-first-child", daemon.MsgCancelTask, &daemon.CancelTaskPayload{
		ID:     first.Task.ID,
		Reason: "release next serial child",
	})
	cancelResp := sendAndRead(t, connOrch, scannerOrch, cancelEnv)
	if cancelResp.Type != daemon.MsgResponse {
		t.Fatalf("expected cancel_task response, got %s", cancelResp.Type)
	}

	if !scannerWorker.Scan() {
		t.Fatal("expected second child push after terminal first child")
	}
	var pushEnv daemon.Envelope
	if err := json.Unmarshal(scannerWorker.Bytes(), &pushEnv); err != nil {
		t.Fatalf("unmarshal second child push: %v", err)
	}
	if pushEnv.Type != daemon.MsgPushMessage {
		t.Fatalf("expected push_message for second child, got %s", pushEnv.Type)
	}

	readEnv2, _ := daemon.NewEnvelope("read-serial-second", daemon.MsgReadMessages, &daemon.ReadMessagesPayload{Limit: 10})
	readResp2 := sendAndRead(t, connWorker, scannerWorker, readEnv2)
	if readResp2.Type != daemon.MsgResponse {
		t.Fatalf("expected read_messages response after release, got %s", readResp2.Type)
	}
	var readPayload2 daemon.ResponsePayload
	if err := readResp2.DecodePayload(&readPayload2); err != nil {
		t.Fatalf("decode released read payload: %v", err)
	}
	if err := json.Unmarshal(readPayload2.Data, &readMessages); err != nil {
		t.Fatalf("unmarshal released read response: %v", err)
	}
	if len(readMessages.Messages) != 1 || readMessages.Messages[0].TaskID != second.Task.ID {
		t.Fatalf("expected released second child only, got %+v", readMessages.Messages)
	}
}

func TestDaemonFreshTaskDispatchDeliversToWorkerRegisteredAfterTaskCreate(t *testing.T) {
	socketPath, cancel := startTestDaemon(t)
	defer cancel()

	connOrch, scannerOrch := connectAndRegister(t, socketPath, "orchestrator")
	defer connOrch.Close()

	taskID := createTaskWithStartMode(t, connOrch, scannerOrch, "worker", "fresh")
	time.Sleep(10 * time.Millisecond)

	connWorker, scannerWorker := connectAndRegister(t, socketPath, "worker")
	defer connWorker.Close()

	message := "Task ID: " + taskID + "\nDispatch after fresh worker start"
	sendEnv, _ := daemon.NewEnvelope("send-fresh-direct", daemon.MsgSendMessage, &daemon.SendMessagePayload{
		To:      "worker",
		Message: message,
	})
	sendResp := sendAndRead(t, connOrch, scannerOrch, sendEnv)
	if sendResp.Type != daemon.MsgResponse {
		t.Fatalf("expected send_message response, got %s", sendResp.Type)
	}

	if !scannerWorker.Scan() {
		t.Fatal("expected immediate push for worker registered after task creation")
	}
	var pushEnv daemon.Envelope
	if err := json.Unmarshal(scannerWorker.Bytes(), &pushEnv); err != nil {
		t.Fatalf("unmarshal push: %v", err)
	}
	if pushEnv.Type != daemon.MsgPushMessage {
		t.Fatalf("expected push_message, got %s", pushEnv.Type)
	}

	readEnv, _ := daemon.NewEnvelope("read-fresh-direct", daemon.MsgReadMessages, &daemon.ReadMessagesPayload{Limit: 10})
	readResp := sendAndRead(t, connWorker, scannerWorker, readEnv)
	if readResp.Type != daemon.MsgResponse {
		t.Fatalf("expected read_messages response, got %s", readResp.Type)
	}
	var readPayload daemon.ResponsePayload
	if err := readResp.DecodePayload(&readPayload); err != nil {
		t.Fatalf("decode read payload: %v", err)
	}
	var readMessages daemon.ReadMessagesResponse
	if err := json.Unmarshal(readPayload.Data, &readMessages); err != nil {
		t.Fatalf("unmarshal read response: %v", err)
	}
	if len(readMessages.Messages) != 1 || readMessages.Messages[0].TaskID != taskID {
		t.Fatalf("expected immediate delivery once worker connection is newer than task, got %+v", readMessages.Messages)
	}
}

func TestDaemonRegisterRehydratesRunnableTaskReminderFromTaskStore(t *testing.T) {
	socketPath, cancel := startTestDaemon(t)
	defer cancel()

	connOrch, scannerOrch := connectAndRegister(t, socketPath, "orchestrator")
	defer connOrch.Close()

	connWorker, scannerWorker := connectAndRegister(t, socketPath, "worker")

	taskID := createTaskWithStartMode(t, connOrch, scannerOrch, "worker", "default")
	message := "Task ID: " + taskID + "\nPlease inspect the daemon task model"
	sendEnv, _ := daemon.NewEnvelope("send-task-rehydrate", daemon.MsgSendMessage, &daemon.SendMessagePayload{
		To:      "worker",
		Message: message,
	})
	sendResp := sendAndRead(t, connOrch, scannerOrch, sendEnv)
	if sendResp.Type != daemon.MsgResponse {
		t.Fatalf("expected send_message response, got %s", sendResp.Type)
	}

	if !scannerWorker.Scan() {
		t.Fatal("expected initial push notification")
	}

	readEnv, _ := daemon.NewEnvelope("read-before-rehydrate", daemon.MsgReadMessages, &daemon.ReadMessagesPayload{Limit: 10})
	readResp := sendAndRead(t, connWorker, scannerWorker, readEnv)
	if readResp.Type != daemon.MsgResponse {
		t.Fatalf("expected read_messages response, got %s", readResp.Type)
	}
	var readPayload daemon.ResponsePayload
	if err := readResp.DecodePayload(&readPayload); err != nil {
		t.Fatalf("decode read payload: %v", err)
	}
	var readMessages daemon.ReadMessagesResponse
	if err := json.Unmarshal(readPayload.Data, &readMessages); err != nil {
		t.Fatalf("unmarshal read response: %v", err)
	}
	if len(readMessages.Messages) != 1 || readMessages.Messages[0].TaskID != taskID {
		t.Fatalf("expected initial task dispatch to be consumed, got %+v", readMessages.Messages)
	}

	_ = connWorker.Close()

	connWorker2, scannerWorker2 := connectAndRegister(t, socketPath, "worker")
	defer connWorker2.Close()

	readResp = sendAndRead(t, connWorker2, scannerWorker2, readEnv)
	if readResp.Type != daemon.MsgResponse {
		t.Fatalf("expected read_messages response after re-register, got %s", readResp.Type)
	}
	if err := readResp.DecodePayload(&readPayload); err != nil {
		t.Fatalf("decode rehydrated read payload: %v", err)
	}
	if err := json.Unmarshal(readPayload.Data, &readMessages); err != nil {
		t.Fatalf("unmarshal rehydrated read response: %v", err)
	}
	if len(readMessages.Messages) != 1 || readMessages.Messages[0].TaskID != taskID {
		t.Fatalf("expected daemon to rehydrate runnable task reminder, got %+v", readMessages.Messages)
	}
	if !strings.Contains(readMessages.Messages[0].Content, "daemon task registry still shows this task as runnable") {
		t.Fatalf("expected canonical reminder message, got %q", readMessages.Messages[0].Content)
	}
}

func TestDaemonSuppressesExactDuplicateTaskReports(t *testing.T) {
	socketPath, cancel := startTestDaemon(t)
	defer cancel()

	conn1, scanner1 := connectAndRegister(t, socketPath, "worker")
	defer conn1.Close()

	conn2, scanner2 := connectAndRegister(t, socketPath, "orchestrator")
	defer conn2.Close()

	message := "Task ID: 11111111-1111-1111-1111-111111111111\nImplemented fix\nVerification: pass"
	sendEnv1, _ := daemon.NewEnvelope("send-report-1", daemon.MsgSendMessage, &daemon.SendMessagePayload{
		To:      "orchestrator",
		Message: message,
	})
	sendEnv2, _ := daemon.NewEnvelope("send-report-2", daemon.MsgSendMessage, &daemon.SendMessagePayload{
		To:      "orchestrator",
		Message: message,
	})

	resp1 := sendAndRead(t, conn1, scanner1, sendEnv1)
	resp2 := sendAndRead(t, conn1, scanner1, sendEnv2)
	if resp1.Type != daemon.MsgResponse || resp2.Type != daemon.MsgResponse {
		t.Fatalf("expected normal responses, got %s and %s", resp1.Type, resp2.Type)
	}

	var payload daemon.ResponsePayload
	resp2.DecodePayload(&payload)
	var result map[string]string
	if err := json.Unmarshal(payload.Data, &result); err != nil {
		t.Fatalf("unmarshal suppressed response: %v", err)
	}
	if result["status"] != "suppressed" {
		t.Fatalf("expected duplicate task report to be suppressed, got %#v", result)
	}

	if !scanner2.Scan() {
		t.Fatal("expected only the first push notification")
	}

	readEnv, _ := daemon.NewEnvelope("read-report", daemon.MsgReadMessages, &daemon.ReadMessagesPayload{Limit: 10})
	readResp := sendAndRead(t, conn2, scanner2, readEnv)
	var readPayload daemon.ResponsePayload
	readResp.DecodePayload(&readPayload)
	var readMessages daemon.ReadMessagesResponse
	if err := json.Unmarshal(readPayload.Data, &readMessages); err != nil {
		t.Fatalf("unmarshal read messages: %v", err)
	}
	if len(readMessages.Messages) != 1 {
		t.Fatalf("expected only one delivered report, got %d", len(readMessages.Messages))
	}
}

func TestDaemonReconnectPreservesLatestWorkspaceRegistration(t *testing.T) {
	socketPath, cancel := startTestDaemon(t)
	defer cancel()

	connOrch, scannerOrch := connectAndRegister(t, socketPath, "orchestrator")
	defer connOrch.Close()

	connOld, _ := connectAndRegister(t, socketPath, "worker")
	connNew, _ := connectAndRegister(t, socketPath, "worker")
	defer connNew.Close()

	if err := connOld.Close(); err != nil {
		t.Fatalf("close old worker connection: %v", err)
	}

	var listResp daemon.ListWorkspacesResponse
	deadline := time.Now().Add(2 * time.Second)
	for {
		listEnv, _ := daemon.NewEnvelope("list-reconnect", daemon.MsgListWorkspaces, struct{}{})
		resp := sendAndRead(t, connOrch, scannerOrch, listEnv)
		if resp.Type != daemon.MsgResponse {
			t.Fatalf("expected response, got %s", resp.Type)
		}

		var payload daemon.ResponsePayload
		if err := resp.DecodePayload(&payload); err != nil {
			t.Fatalf("decode list payload: %v", err)
		}
		if err := json.Unmarshal(payload.Data, &listResp); err != nil {
			t.Fatalf("unmarshal list response: %v", err)
		}

		workerCount := 0
		for _, ws := range listResp.Workspaces {
			if ws.Name == "worker" {
				workerCount++
			}
		}
		if workerCount == 1 && len(listResp.Workspaces) == 2 {
			break
		}
		if time.Now().After(deadline) {
			t.Fatalf("expected reconnect to preserve one active worker registration, got %+v", listResp.Workspaces)
		}
		time.Sleep(20 * time.Millisecond)
	}
}

// TestDaemonBroadcastDoesNotStallOnSlowReceiver verifies that one stalled
// recipient (a registered workspace whose process never reads its socket)
// cannot block the daemon from delivering broadcast pushes to other
// healthy recipients. Before the per-connection async writer goroutine
// was introduced, the broadcast loop performed synchronous writes while
// holding the recipient's lock, which could stall the daemon entirely.
func TestDaemonBroadcastDoesNotStallOnSlowReceiver(t *testing.T) {
	socketPath, cancel := startTestDaemon(t)
	defer cancel()

	connOrch, scannerOrch := connectAndRegister(t, socketPath, "orchestrator")
	defer connOrch.Close()

	// "slow" registers but never reads from its socket, simulating an
	// unresponsive worker.
	connSlow, _ := connectAndRegister(t, socketPath, "slow")
	defer connSlow.Close()

	connFast, scannerFast := connectAndRegister(t, socketPath, "fast")
	defer connFast.Close()

	broadcastEnv, _ := daemon.NewEnvelope("bcast-1", daemon.MsgBroadcast, &daemon.BroadcastPayload{
		Message: "deploy v2 starting",
	})

	done := make(chan *daemon.Envelope, 1)
	go func() {
		done <- sendAndRead(t, connOrch, scannerOrch, broadcastEnv)
	}()

	select {
	case resp := <-done:
		if resp.Type != daemon.MsgResponse {
			t.Fatalf("expected broadcast response, got %s", resp.Type)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("broadcast send blocked on slow receiver")
	}

	// The fast worker must still receive its broadcast push promptly.
	pushDone := make(chan daemon.Envelope, 1)
	go func() {
		if scannerFast.Scan() {
			var env daemon.Envelope
			_ = json.Unmarshal(scannerFast.Bytes(), &env)
			pushDone <- env
		}
	}()
	select {
	case env := <-pushDone:
		if env.Type != daemon.MsgPushMessage {
			t.Fatalf("expected push_message to fast worker, got %s", env.Type)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("fast worker did not receive broadcast push in time")
	}
}

func TestDaemonRestartRehydratesQueueTasksHistoryAndSharedValues(t *testing.T) {
	dir, err := os.MkdirTemp("", "axd-restart-")
	if err != nil {
		t.Fatalf("mkdtemp: %v", err)
	}
	defer os.RemoveAll(dir)
	socketPath := filepath.Join(dir, "test.sock")

	cancelFirst, doneFirst := startTestDaemonAt(t, socketPath)

	connOrch, scannerOrch := connectAndRegister(t, socketPath, "orchestrator")

	setSharedEnv, _ := daemon.NewEnvelope("set-shared", daemon.MsgSetShared, &daemon.SetSharedPayload{
		Key:   "api_url",
		Value: "http://localhost:8080",
	})
	setSharedResp := sendAndRead(t, connOrch, scannerOrch, setSharedEnv)
	if setSharedResp.Type != daemon.MsgResponse {
		t.Fatalf("expected set_shared response, got %s", setSharedResp.Type)
	}

	createTaskEnv, _ := daemon.NewEnvelope("create-task", daemon.MsgCreateTask, &daemon.CreateTaskPayload{
		Title:       "Investigate restart durability",
		Description: "Ensure state survives daemon restart",
		Assignee:    "worker",
	})
	createTaskResp := sendAndRead(t, connOrch, scannerOrch, createTaskEnv)
	if createTaskResp.Type != daemon.MsgResponse {
		t.Fatalf("expected create_task response, got %s", createTaskResp.Type)
	}
	var createTaskPayload daemon.ResponsePayload
	if err := createTaskResp.DecodePayload(&createTaskPayload); err != nil {
		t.Fatalf("decode create task payload: %v", err)
	}
	var taskResp daemon.TaskResponse
	if err := json.Unmarshal(createTaskPayload.Data, &taskResp); err != nil {
		t.Fatalf("unmarshal create task response: %v", err)
	}

	message := "Task ID: " + taskResp.Task.ID + "\nImplemented fix\nVerification: pass"
	sendMessageEnv, _ := daemon.NewEnvelope("send-before-restart", daemon.MsgSendMessage, &daemon.SendMessagePayload{
		To:      "worker",
		Message: message,
	})
	sendResp := sendAndRead(t, connOrch, scannerOrch, sendMessageEnv)
	if sendResp.Type != daemon.MsgResponse {
		t.Fatalf("expected send_message response, got %s", sendResp.Type)
	}

	_ = connOrch.Close()
	cancelFirst()
	<-doneFirst

	cancelSecond, doneSecond := startTestDaemonAt(t, socketPath)
	defer func() {
		cancelSecond()
		<-doneSecond
	}()

	connOrch2, scannerOrch2 := connectAndRegister(t, socketPath, "orchestrator")
	defer connOrch2.Close()

	getSharedEnv, _ := daemon.NewEnvelope("get-shared", daemon.MsgGetShared, &daemon.GetSharedPayload{
		Key: "api_url",
	})
	getSharedResp := sendAndRead(t, connOrch2, scannerOrch2, getSharedEnv)
	if getSharedResp.Type != daemon.MsgResponse {
		t.Fatalf("expected get_shared response, got %s", getSharedResp.Type)
	}
	var getSharedPayload daemon.ResponsePayload
	if err := getSharedResp.DecodePayload(&getSharedPayload); err != nil {
		t.Fatalf("decode get shared payload: %v", err)
	}
	var getSharedResult daemon.GetSharedResponse
	if err := json.Unmarshal(getSharedPayload.Data, &getSharedResult); err != nil {
		t.Fatalf("unmarshal get shared response: %v", err)
	}
	if !getSharedResult.Found || getSharedResult.Value != "http://localhost:8080" {
		t.Fatalf("shared value not rehydrated: %+v", getSharedResult)
	}

	getTaskEnv, _ := daemon.NewEnvelope("get-task", daemon.MsgGetTask, &daemon.GetTaskPayload{
		ID: taskResp.Task.ID,
	})
	getTaskResp := sendAndRead(t, connOrch2, scannerOrch2, getTaskEnv)
	if getTaskResp.Type != daemon.MsgResponse {
		t.Fatalf("expected get_task response, got %s", getTaskResp.Type)
	}
	var getTaskPayload daemon.ResponsePayload
	if err := getTaskResp.DecodePayload(&getTaskPayload); err != nil {
		t.Fatalf("decode get task payload: %v", err)
	}
	var getTaskResult daemon.TaskResponse
	if err := json.Unmarshal(getTaskPayload.Data, &getTaskResult); err != nil {
		t.Fatalf("unmarshal get task response: %v", err)
	}
	if getTaskResult.Task.ID != taskResp.Task.ID || getTaskResult.Task.Title != taskResp.Task.Title {
		t.Fatalf("task not rehydrated: %+v", getTaskResult.Task)
	}

	sendDuplicateEnv, _ := daemon.NewEnvelope("send-after-restart", daemon.MsgSendMessage, &daemon.SendMessagePayload{
		To:      "worker",
		Message: message,
	})
	sendDuplicateResp := sendAndRead(t, connOrch2, scannerOrch2, sendDuplicateEnv)
	if sendDuplicateResp.Type != daemon.MsgResponse {
		t.Fatalf("expected duplicate send response, got %s", sendDuplicateResp.Type)
	}
	var duplicatePayload daemon.ResponsePayload
	if err := sendDuplicateResp.DecodePayload(&duplicatePayload); err != nil {
		t.Fatalf("decode duplicate send payload: %v", err)
	}
	var duplicateResult map[string]string
	if err := json.Unmarshal(duplicatePayload.Data, &duplicateResult); err != nil {
		t.Fatalf("unmarshal duplicate send result: %v", err)
	}
	if duplicateResult["status"] != "suppressed" {
		t.Fatalf("expected history-backed suppression after restart, got %#v", duplicateResult)
	}

	connWorker, scannerWorker := connectAndRegister(t, socketPath, "worker")
	defer connWorker.Close()

	readMessagesEnv, _ := daemon.NewEnvelope("read-worker", daemon.MsgReadMessages, &daemon.ReadMessagesPayload{Limit: 10})
	readMessagesResp := sendAndRead(t, connWorker, scannerWorker, readMessagesEnv)
	if readMessagesResp.Type != daemon.MsgResponse {
		t.Fatalf("expected read_messages response, got %s", readMessagesResp.Type)
	}
	var readMessagesPayload daemon.ResponsePayload
	if err := readMessagesResp.DecodePayload(&readMessagesPayload); err != nil {
		t.Fatalf("decode read messages payload: %v", err)
	}
	var readMessagesResult daemon.ReadMessagesResponse
	if err := json.Unmarshal(readMessagesPayload.Data, &readMessagesResult); err != nil {
		t.Fatalf("unmarshal read messages response: %v", err)
	}
	if len(readMessagesResult.Messages) != 1 || readMessagesResult.Messages[0].Content != message {
		t.Fatalf("queue not rehydrated correctly: %+v", readMessagesResult.Messages)
	}
}
