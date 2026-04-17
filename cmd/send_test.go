package cmd

import (
	"testing"

	"github.com/ashon/ax/internal/mcpserver"
)

type stubSendClient struct {
	connected   bool
	closed      bool
	sendTo      string
	sendMsg     string
	sendConfig  string
	result      *mcpserver.SendMessageResult
	err         error
}

func (c *stubSendClient) Connect() error {
	c.connected = true
	return nil
}

func (c *stubSendClient) Close() error {
	c.closed = true
	return nil
}

func (c *stubSendClient) SendMessage(to, message, configPath string) (*mcpserver.SendMessageResult, error) {
	c.sendTo = to
	c.sendMsg = message
	c.sendConfig = configPath
	return c.result, c.err
}

func TestSendCommandDispatchesQueuedWorkOnDemand(t *testing.T) {
	oldSocketPath := socketPath
	oldSendNewClient := sendNewClient
	oldSendResolveConfigPath := sendResolveConfigPath
	t.Cleanup(func() {
		socketPath = oldSocketPath
		sendNewClient = oldSendNewClient
		sendResolveConfigPath = oldSendResolveConfigPath
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
	if client.sendConfig != "/tmp/test-config.yaml" {
		t.Fatalf("expected send to carry dispatch config path, got %q", client.sendConfig)
	}
}
