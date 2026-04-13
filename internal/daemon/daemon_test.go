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

	"github.com/ashon/ax/internal/daemon"
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
