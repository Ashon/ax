package daemon

import (
	"net"
	"testing"
	"time"

	"github.com/ashon/ax/internal/types"
)

func TestConnEntrySendDeliversAndClose(t *testing.T) {
	clientConn, serverConn := net.Pipe()
	defer clientConn.Close()
	defer serverConn.Close()

	r := NewRegistry()
	entry, previous := r.Register("worker", "/tmp", "test", "/tmp/.ax/config.yaml", 15*time.Minute, clientConn)
	if previous != nil {
		t.Fatalf("expected no previous entry, got %+v", previous)
	}
	if entry == nil {
		t.Fatal("expected entry")
	}

	env, _ := NewEnvelope("e1", MsgPushMessage, map[string]string{"hello": "world"})
	if !entry.Send(env, time.Second) {
		t.Fatal("Send returned false on a fresh entry")
	}

	// Close should make subsequent sends return false promptly.
	entry.Close()

	start := time.Now()
	if entry.Send(env, time.Second) {
		t.Fatal("Send returned true after Close")
	}
	if time.Since(start) > 100*time.Millisecond {
		t.Fatalf("Send after Close should return immediately, took %s", time.Since(start))
	}
}

func TestConnEntrySendTimesOutWhenOutboxFull(t *testing.T) {
	clientConn, serverConn := net.Pipe()
	defer clientConn.Close()
	defer serverConn.Close()

	entry := newConnEntry(types.WorkspaceInfo{Name: "worker"}, "", 0, time.Now(), clientConn)
	defer entry.Close()

	env, _ := NewEnvelope("e", MsgPushMessage, map[string]string{"k": "v"})

	// Fill the outbox without a writer goroutine attached so Send blocks.
	for i := 0; i < outboxCapacity; i++ {
		if !entry.Send(env, time.Second) {
			t.Fatalf("unexpected Send failure at %d", i)
		}
	}

	start := time.Now()
	if entry.Send(env, 50*time.Millisecond) {
		t.Fatal("Send should fail when outbox is full and writer is absent")
	}
	if elapsed := time.Since(start); elapsed > 250*time.Millisecond {
		t.Fatalf("Send should respect timeout, took %s", elapsed)
	}
}

func TestRegistryUnregisterIfConnClosesEntry(t *testing.T) {
	clientConn, serverConn := net.Pipe()
	defer clientConn.Close()
	defer serverConn.Close()

	r := NewRegistry()
	entry, _ := r.Register("worker", "", "", "", 0, clientConn)
	if !r.UnregisterIfConn("worker", clientConn) {
		t.Fatal("expected UnregisterIfConn to succeed")
	}

	select {
	case <-entry.closeCh:
		// entry signalled closed
	case <-time.After(time.Second):
		t.Fatal("entry was not closed by UnregisterIfConn")
	}
}
