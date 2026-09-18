package main

import (
	"bytes"
	"context"
	"io"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"tailscale.com/logtail"
	"tailscale.com/logtail/filch"
)

const syntheticAuthURL = "https://login.tailscale.com/a/not-a-real-key-logging-fixture"

// Disable is process-global and irreversible. Keep its production invocation
// separate from other tests and do not inherit any account/debug environment.
func isolatedLoggingTest(t *testing.T) bool {
	t.Helper()
	if os.Getenv("NEIGE_TAILNET_LOGGING_TEST") == t.Name() {
		return true
	}
	executable, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, executable, "-test.run=^"+t.Name()+"$", "-test.count=1")
	cmd.Env = []string{"NEIGE_TAILNET_LOGGING_TEST=" + t.Name(), "PATH=/usr/bin:/bin", "LANG=C.UTF-8", "TMPDIR=" + t.TempDir()}
	if output, err := cmd.CombinedOutput(); err != nil {
		t.Fatalf("isolated logging check: %v\n%s", err, output)
	}
	return false
}

type observedFilch struct {
	*filch.Filch
	writes atomic.Int64
}

func (f *observedFilch) Write(p []byte) (int, error) {
	f.writes.Add(1)
	return f.Filch.Write(p)
}

type uploadSink struct{ requests atomic.Int64 }

func (s *uploadSink) RoundTrip(r *http.Request) (*http.Response, error) {
	s.requests.Add(1)
	_, err := io.Copy(io.Discard, r.Body)
	if err != nil {
		return nil, err
	}
	return &http.Response{StatusCode: http.StatusOK, Body: io.NopCloser(strings.NewReader("")), Header: make(http.Header)}, nil
}

func exerciseLogtail(t *testing.T, prefix string) (*observedFilch, *uploadSink) {
	t.Helper()
	f, err := filch.New(prefix, filch.Options{ReplaceStderr: false})
	if err != nil {
		t.Fatal(err)
	}
	buffer := &observedFilch{Filch: f}
	sink := &uploadSink{}
	// Calling NewLogger directly matters: tsnet.startLogger skips its entire
	// logging path under go test, which would make a Server.Start test vacuous.
	logger := logtail.NewLogger(logtail.Config{
		Collection: logtail.CollectionNode, Buffer: buffer, Stderr: io.Discard,
		HTTPC:        &http.Client{Transport: sink},
		FlushDelayFn: func() time.Duration { return 0 },
	}, func(string, ...any) {})
	logger.Logf("To authorize the fixture, go to: %s", syntheticAuthURL)
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if err := logger.Shutdown(ctx); err != nil {
		t.Error(err)
	}
	if err := f.Close(); err != nil {
		t.Error(err)
	}
	return buffer, sink
}

func TestPrivateServerSuppressesNewLogtailEntries(t *testing.T) {
	if !isolatedLoggingTest(t) {
		return
	}
	dir := t.TempDir()
	server := privateTailnetServer(dir, "logging-fixture")
	server.UserLogf("%s", syntheticAuthURL)
	server.Logf("%s", syntheticAuthURL)
	buffer, sink := exerciseLogtail(t, filepath.Join(dir, "tailscaled"))
	if writes := buffer.writes.Load(); writes != 0 {
		t.Errorf("new log entries reached the real filch buffer: %d writes", writes)
	}
	if requests := sink.requests.Load(); requests != 0 {
		t.Errorf("new log entries reached the upload sink: %d requests", requests)
	}
	files, err := filepath.Glob(filepath.Join(dir, "tailscaled*"))
	if err != nil {
		t.Fatal(err)
	}
	for _, file := range files {
		contents, err := os.ReadFile(file)
		if err != nil {
			t.Fatal(err)
		}
		if bytes.Contains(contents, []byte(syntheticAuthURL)) {
			t.Error("synthetic login URL persisted in log buffer")
		}
	}
}
