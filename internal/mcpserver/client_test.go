package mcpserver

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"net"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/ashon/ax/internal/daemon"
	"github.com/ashon/ax/internal/types"
)

func TestSplitBufferedMessagesKeepsUnmatchedPushes(t *testing.T) {
	now := time.Now()
	buffered := []types.Message{
		{ID: "1", From: "orch", Content: "task", CreatedAt: now},
		{ID: "2", From: "peer", Content: "note", CreatedAt: now.Add(time.Second)},
	}

	matched, remaining := splitBufferedMessages(buffered, "orch")
	if len(matched) != 1 || matched[0].ID != "1" {
		t.Fatalf("expected only orch message to match, got %+v", matched)
	}
	if len(remaining) != 1 || remaining[0].ID != "2" {
		t.Fatalf("expected unmatched push to remain buffered, got %+v", remaining)
	}
}

func TestMergeUniqueMessagesDeduplicatesPushAndPullCopies(t *testing.T) {
	now := time.Now()
	pushed := []types.Message{
		{ID: "dup", From: "orch", Content: "first", CreatedAt: now},
	}
	pulled := []types.Message{
		{ID: "dup", From: "orch", Content: "first", CreatedAt: now},
		{ID: "next", From: "orch", Content: "second", CreatedAt: now.Add(time.Second)},
	}

	merged := mergeUniqueMessages(pushed, pulled)
	if len(merged) != 2 {
		t.Fatalf("expected deduplicated message list, got %+v", merged)
	}
	if merged[0].ID != "dup" || merged[1].ID != "next" {
		t.Fatalf("unexpected ordering/content after merge: %+v", merged)
	}
}

func TestSendRequestReturnsResponse(t *testing.T) {
	clientConn, serverConn := net.Pipe()
	defer clientConn.Close()
	defer serverConn.Close()

	client := NewDaemonClient("", "worker")
	client.conn = clientConn
	client.connected.Store(true)
	client.setDisconnectErr(nil)

	go client.readLoop()

	serverErr := make(chan error, 1)
	go func() {
		scanner := bufio.NewScanner(serverConn)
		if !scanner.Scan() {
			serverErr <- scanner.Err()
			return
		}

		var env daemon.Envelope
		if err := json.Unmarshal(scanner.Bytes(), &env); err != nil {
			serverErr <- err
			return
		}

		resp, err := daemon.NewResponseEnvelope(env.ID, map[string]string{"status": "ok"})
		if err != nil {
			serverErr <- err
			return
		}
		data, err := json.Marshal(resp)
		if err != nil {
			serverErr <- err
			return
		}
		_, err = serverConn.Write(append(data, '\n'))
		serverErr <- err
	}()

	resp, err := client.sendRequest(daemon.MsgListWorkspaces, struct{}{})
	if err != nil {
		t.Fatalf("sendRequest returned error: %v", err)
	}
	if resp.Type != daemon.MsgResponse {
		t.Fatalf("expected response envelope, got %s", resp.Type)
	}

	if err := <-serverErr; err != nil {
		t.Fatalf("server goroutine failed: %v", err)
	}
}

func TestDecodeResponseDataPropagatesUnmarshalError(t *testing.T) {
	resp := &daemon.Envelope{
		ID:      "abc",
		Type:    daemon.MsgResponse,
		Payload: json.RawMessage(`{"success":true,"data":"not-json"}`),
	}
	var dst daemon.ListWorkspacesResponse
	err := decodeResponseData(resp, &dst)
	if err == nil {
		t.Fatal("expected decode error for malformed payload, got nil")
	}
	if !strings.Contains(err.Error(), "unmarshal response data") {
		t.Fatalf("expected unmarshal error to be wrapped, got %v", err)
	}
}

func TestDecodeResponseDataAcceptsEmptyData(t *testing.T) {
	resp := &daemon.Envelope{
		ID:      "abc",
		Type:    daemon.MsgResponse,
		Payload: json.RawMessage(`{"success":true}`),
	}
	var dst daemon.ListWorkspacesResponse
	if err := decodeResponseData(resp, &dst); err != nil {
		t.Fatalf("expected nil error for empty data, got %v", err)
	}
}

func TestDecodeResponseDataRejectsNilEnvelope(t *testing.T) {
	var dst daemon.ListWorkspacesResponse
	if err := decodeResponseData(nil, &dst); err == nil {
		t.Fatal("expected error for nil envelope")
	}
}

func TestSendRequestRespectsContextDeadline(t *testing.T) {
	clientConn, serverConn := net.Pipe()
	defer clientConn.Close()
	defer serverConn.Close()

	client := NewDaemonClient("", "worker")
	client.conn = clientConn
	client.connected.Store(true)
	client.setDisconnectErr(nil)
	client.SetRequestTimeout(0) // rely solely on the supplied context

	go client.readLoop()

	// Drain whatever the client writes so it doesn't block on the pipe.
	go func() {
		scanner := bufio.NewScanner(serverConn)
		scanner.Buffer(make([]byte, 1024*1024), 1024*1024)
		for scanner.Scan() {
			// never reply
		}
	}()

	ctx, cancel := context.WithTimeout(context.Background(), 100*time.Millisecond)
	defer cancel()

	start := time.Now()
	_, err := client.sendRequestCtx(ctx, daemon.MsgListWorkspaces, struct{}{})
	if err == nil {
		t.Fatal("expected sendRequestCtx to fail when the context expires")
	}
	if !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("expected DeadlineExceeded, got %v", err)
	}
	elapsed := time.Since(start)
	if elapsed > 500*time.Millisecond {
		t.Fatalf("sendRequestCtx took %s, expected ~100ms", elapsed)
	}

	// The pending entry must be cleaned up so it cannot leak.
	client.pendingMu.Lock()
	pendingCount := len(client.pending)
	client.pendingMu.Unlock()
	if pendingCount != 0 {
		t.Fatalf("expected pending map to be empty after timeout, got %d entries", pendingCount)
	}
}

func TestSendRequestAppliesDefaultTimeout(t *testing.T) {
	clientConn, serverConn := net.Pipe()
	defer clientConn.Close()
	defer serverConn.Close()

	client := NewDaemonClient("", "worker")
	client.conn = clientConn
	client.connected.Store(true)
	client.setDisconnectErr(nil)
	client.SetRequestTimeout(150 * time.Millisecond)

	go client.readLoop()
	go func() {
		scanner := bufio.NewScanner(serverConn)
		scanner.Buffer(make([]byte, 1024*1024), 1024*1024)
		for scanner.Scan() {
		}
	}()

	start := time.Now()
	_, err := client.sendRequest(daemon.MsgListWorkspaces, struct{}{})
	if err == nil {
		t.Fatal("expected sendRequest to fail when default timeout elapses")
	}
	if !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("expected DeadlineExceeded from default timeout, got %v", err)
	}
	if elapsed := time.Since(start); elapsed > 600*time.Millisecond {
		t.Fatalf("default timeout did not fire promptly, elapsed %s", elapsed)
	}
}

func TestSendRequestFailsPendingWaitersOnDisconnect(t *testing.T) {
	clientConn, serverConn := net.Pipe()
	defer clientConn.Close()

	client := NewDaemonClient("", "worker")
	client.conn = clientConn
	client.connected.Store(true)
	client.setDisconnectErr(nil)

	go client.readLoop()

	const requestCount = 2
	results := make(chan error, requestCount)
	var wg sync.WaitGroup
	for i := 0; i < requestCount; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			_, err := client.sendRequest(daemon.MsgListWorkspaces, struct{}{})
			results <- err
		}()
	}

	scanner := bufio.NewScanner(serverConn)
	for i := 0; i < requestCount; i++ {
		if !scanner.Scan() {
			t.Fatalf("expected request %d to reach server side, scanner err=%v", i+1, scanner.Err())
		}
	}

	if err := serverConn.Close(); err != nil {
		t.Fatalf("close server conn: %v", err)
	}

	wgDone := make(chan struct{})
	go func() {
		wg.Wait()
		close(wgDone)
	}()

	select {
	case <-wgDone:
	case <-time.After(1 * time.Second):
		t.Fatal("pending sendRequest calls did not finish after disconnect")
	}

	for i := 0; i < requestCount; i++ {
		err := <-results
		if err == nil {
			t.Fatalf("expected disconnect error for request %d", i+1)
		}
		if !strings.Contains(err.Error(), "daemon connection closed") {
			t.Fatalf("expected concrete disconnect error, got %v", err)
		}
	}

	start := time.Now()
	_, err := client.sendRequest(daemon.MsgListWorkspaces, struct{}{})
	if err == nil {
		t.Fatal("expected immediate error after disconnect")
	}
	if !strings.Contains(err.Error(), "daemon connection closed") {
		t.Fatalf("expected concrete disconnect error after disconnect, got %v", err)
	}
	if time.Since(start) > 100*time.Millisecond {
		t.Fatalf("post-disconnect sendRequest should fail immediately, took %s", time.Since(start))
	}
}
