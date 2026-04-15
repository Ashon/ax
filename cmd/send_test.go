package cmd

import (
	"testing"

	"github.com/ashon/ax/internal/mcpserver"
)

type stubSendClient struct {
	connected bool
	closed    bool
	sendTo    string
	sendMsg   string
	result    *mcpserver.SendMessageResult
	err       error
}

func (c *stubSendClient) Connect() error {
	c.connected = true
	return nil
}

func (c *stubSendClient) Close() error {
	c.closed = true
	return nil
}

func (c *stubSendClient) SendMessage(to, message string) (*mcpserver.SendMessageResult, error) {
	c.sendTo = to
	c.sendMsg = message
	return c.result, c.err
}

func TestSendCommandDispatchesQueuedWorkOnDemand(t *testing.T) {
	oldSocketPath := socketPath
	oldSendNewClient := sendNewClient
	oldSendResolveConfigPath := sendResolveConfigPath
	oldSendDispatchRunnableWork := sendDispatchRunnableWork
	t.Cleanup(func() {
		socketPath = oldSocketPath
		sendNewClient = oldSendNewClient
		sendResolveConfigPath = oldSendResolveConfigPath
		sendDispatchRunnableWork = oldSendDispatchRunnableWork
	})

	socketPath = "/tmp/ax.sock"
	client := &stubSendClient{
		result: &mcpserver.SendMessageResult{
			MessageID: "msg-1",
		},
	}
	sendNewClient = func(socketPath, workspace string) sendClient {
		if socketPath != "/tmp/ax.sock" {
			t.Fatalf("unexpected socket path %q", socketPath)
		}
		if workspace != "orchestrator" {
			t.Fatalf("unexpected workspace %q", workspace)
		}
		return client
	}
	sendResolveConfigPath = func() (string, error) {
		return "/tmp/test-config.yaml", nil
	}
	dispatched := false
	sendDispatchRunnableWork = func(socketPath, configPath, target, sender string, fresh bool) error {
		dispatched = true
		if socketPath != "/tmp/ax.sock" {
			t.Fatalf("unexpected dispatch socket path %q", socketPath)
		}
		if configPath != "/tmp/test-config.yaml" {
			t.Fatalf("unexpected dispatch config path %q", configPath)
		}
		if target != "worker" {
			t.Fatalf("unexpected dispatch target %q", target)
		}
		if sender != "orchestrator" {
			t.Fatalf("unexpected dispatch sender %q", sender)
		}
		if fresh {
			t.Fatal("send command should not request fresh dispatch")
		}
		return nil
	}

	if err := sendCmd.RunE(sendCmd, []string{"worker", "hello", "world"}); err != nil {
		t.Fatalf("send command failed: %v", err)
	}

	if !client.connected {
		t.Fatal("expected client to connect")
	}
	if !client.closed {
		t.Fatal("expected client to close")
	}
	if client.sendTo != "worker" || client.sendMsg != "hello world" {
		t.Fatalf("unexpected send payload: to=%q message=%q", client.sendTo, client.sendMsg)
	}
	if !dispatched {
		t.Fatal("expected queued work to be dispatched")
	}
}
