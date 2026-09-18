package main

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestStartupCleanupRemovesCrashedCredentialTemporary(t *testing.T) {
	e, _, _ := enrollmentFixture(t, true)
	path := seedCrashedPending(t, e.dir)
	if err := clearPending(e.dir); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(path); !os.IsNotExist(err) {
		t.Fatal("startup retained credentials from interrupted atomic write")
	}
}

func TestSuccessfulHandoffRemovesCrashedCredentialTemporary(t *testing.T) {
	e, _, qr := enrollmentFixture(t, true)
	path := seedCrashedPending(t, e.dir)
	if _, err := e.enroll(qr, testOperationPermit(t)); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(path); !os.IsNotExist(err) {
		t.Fatal("handoff retained crashed pending credentials")
	}
}

func TestResetRemovesCrashedCredentialTemporary(t *testing.T) {
	e, _, _ := enrollmentFixture(t, true)
	path := seedCrashedPending(t, e.dir)
	if err := e.resetEnrollment(testOperationPermit(t)); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(path); !os.IsNotExist(err) {
		t.Fatal("reset retained crashed pending credentials")
	}
}

func TestCancellationRemovesCrashedCredentialTemporary(t *testing.T) {
	e, _, _ := enrollmentFixture(t, true)
	path := seedCrashedPending(t, e.dir)
	if err := e.cancelEnrollment(); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(path); !os.IsNotExist(err) {
		t.Fatal("cancellation retained crashed pending credentials")
	}
}

func seedCrashedPending(t *testing.T, dir string) string {
	t.Helper()
	// Actual old production CreateTemp namespace, with death after sync and
	// before rename. No fake cleanup implementation is substituted.
	f, err := os.CreateTemp(dir, ".enrollment-*")
	if err != nil {
		t.Fatal(err)
	}
	if _, err := f.WriteString(`{"authKey":"tskey-auth-crash-sentinel","pairTicket":"` + strings.Repeat("c", 64) + `"}`); err != nil {
		t.Fatal(err)
	}
	if err := f.Sync(); err != nil {
		t.Fatal(err)
	}
	if err := f.Close(); err != nil {
		t.Fatal(err)
	}
	return f.Name()
}

func TestCleanupRejectsUnownedTemporaryAndNeverFollowsSymlink(t *testing.T) {
	for _, kind := range []string{"symlink", "mode", "directory"} {
		t.Run(kind, func(t *testing.T) {
			e, node, qr := enrollmentFixture(t, false)
			path := filepath.Join(e.dir, ".enrollment-1234")
			unknown := filepath.Join(e.dir, "unrelated-record")
			if err := os.WriteFile(unknown, []byte("not ours"), 0600); err != nil {
				t.Fatal(err)
			}
			switch kind {
			case "symlink":
				if err := os.Symlink(unknown, path); err != nil {
					t.Fatal(err)
				}
			case "mode":
				if err := os.WriteFile(path, []byte("wrong permissions"), 0644); err != nil {
					t.Fatal(err)
				}
			case "directory":
				if err := os.Mkdir(path, 0700); err != nil {
					t.Fatal(err)
				}
			}
			if _, err := e.enroll(qr, testOperationPermit(t)); err == nil || node.authCalls != 0 {
				t.Fatal("unconfirmed cleanup still allowed enrollment")
			}
			if _, err := os.Lstat(path); err != nil {
				t.Fatal("deleted unowned path")
			}
			if data, err := os.ReadFile(unknown); err != nil || string(data) != "not ours" {
				t.Fatal("modified unknown file")
			}
		})
	}
}

func TestCleanupRejectsNonPrivateNodeDirectory(t *testing.T) {
	e, _, _ := enrollmentFixture(t, true)
	path := seedCrashedPending(t, e.dir)
	if err := os.Chmod(e.dir, 0755); err != nil {
		t.Fatal(err)
	}
	if err := clearPending(e.dir); err == nil {
		t.Fatal("non-private state directory accepted")
	}
	if _, err := os.Stat(path); err != nil {
		t.Fatal("unconfirmed directory contents deleted")
	}
}
