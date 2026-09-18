//go:build ts_omit_logtail

package main

import (
	"bytes"
	"os"
	"path/filepath"
	"testing"
)

func TestReleaseLogtailLeavesExistingBuffersUntouched(t *testing.T) {
	if !isolatedLoggingTest(t) {
		return
	}
	dir := t.TempDir()
	prefix := filepath.Join(dir, "tailscaled")
	oldEntry := []byte(syntheticAuthURL + "\n")
	for _, suffix := range []string{".log1.txt", ".log2.txt"} {
		if err := os.WriteFile(prefix+suffix, oldEntry, 0600); err != nil {
			t.Fatal(err)
		}
	}
	privateTailnetServer(dir, "logging-fixture")
	buffer, sink := exerciseLogtail(t, prefix)
	if buffer.writes.Load() != 0 || sink.requests.Load() != 0 {
		t.Fatal("release logging implementation wrote or uploaded an entry")
	}
	for _, suffix := range []string{".log1.txt", ".log2.txt"} {
		contents, err := os.ReadFile(prefix + suffix)
		if err != nil {
			t.Fatal(err)
		}
		if !bytes.Equal(contents, oldEntry) {
			t.Error("release logging implementation changed an existing buffer")
		}
	}
}
