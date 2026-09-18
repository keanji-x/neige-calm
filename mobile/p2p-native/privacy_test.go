package main

import (
	"context"
	"io"
	"sync/atomic"
	"tailscale.com/logtail"
	"testing"
	"time"
)

type observedLogBuffer struct{ writes atomic.Int64 }

func (b *observedLogBuffer) Write(p []byte) (int, error) { b.writes.Add(1); return len(p), nil }
func (*observedLogBuffer) TryReadLine() ([]byte, error)  { return nil, nil }

func TestPrivateNodeInitializationDisablesActualLogtailBuffer(t *testing.T) {
	// Do not test only tsnet.Server.Start: the SDK skips startLogger under go
	// test. Exercise the production initializer and the real logger it disables.
	_ = privateTailnetNode(t.TempDir(), "privacy-test")
	buffer := &observedLogBuffer{}
	logger := logtail.NewLogger(logtail.Config{BaseURL: "http://127.0.0.1:1", Buffer: buffer, Stderr: io.Discard}, func(string, ...any) {})
	logger.Logf("private enrollment sentinel")
	if buffer.writes.Load() != 0 {
		t.Error("private node initialization allowed logtail to buffer credentials")
	}
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	_ = logger.Shutdown(ctx)
}
