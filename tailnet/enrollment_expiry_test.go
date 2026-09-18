package main

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestEnrollmentExpiredCapacityRetiresOnlyVerifiedSameBinding(t *testing.T) {
	i, s, cmd, c := issuerFixture(t)
	reading := enrollmentClockReading{Wall: time.Now(), BootID: "test-boot", BootNanos: int64(time.Hour)}
	i.clock = func() (enrollmentClockReading, error) { return reading, nil }
	fixtureAPI(t, i, c, 300*time.Second, true)
	if _, err := i.issue(context.Background(), cmd, s); err != nil {
		t.Fatal(err)
	}
	row := i.ledger.Records[0]
	i.ledger.Records = nil
	for n := 0; n < 64; n++ {
		copy := row
		copy.EnrollmentID = fmt.Sprintf("expired-%d", n)
		i.ledger.Records = append(i.ledger.Records, copy)
	}
	if err := i.save(); err != nil {
		t.Fatal(err)
	}
	reading.Wall = reading.Wall.Add(10 * time.Minute)
	reading.BootNanos += int64(10 * time.Minute)
	result, err := i.cleanup(context.Background(), "", true)
	if err != nil || result.PendingCleanup != 0 {
		t.Fatalf("known-expired capacity not retired: count=%d err=%v", result.PendingCleanup, err)
	}
	if len(i.ledger.Records) != 0 {
		t.Fatal("expired rows remain")
	}
	// Return the injected observation to real time for a fresh API fixture.
	reading.Wall = time.Now()
	reading.BootNanos += int64(time.Second)
	cmd.EnrollmentID = "new-attempt"
	cmd.Deadline = time.Now().Add(8 * time.Second).UnixMilli()
	if _, err := i.issue(context.Background(), cmd, s); err != nil {
		t.Fatalf("capacity remains blocked: %v", err)
	}
}

func TestEnrollmentExpiryRetainsUnknownMismatchedAndUnproven(t *testing.T) {
	for _, mode := range []string{"unknown", "binding", "no-evidence", "malformed-expiry", "changed-expiry", "future"} {
		t.Run(mode, func(t *testing.T) {
			i, s, cmd, c := issuerFixture(t)
			reading := enrollmentClockReading{Wall: time.Now(), BootID: "test-boot", BootNanos: int64(time.Hour)}
			i.clock = func() (enrollmentClockReading, error) { return reading, nil }
			fixtureAPI(t, i, c, 300*time.Second, true)
			if _, err := i.issue(context.Background(), cmd, s); err != nil {
				t.Fatal(err)
			}
			r := &i.ledger.Records[0]
			switch mode {
			case "unknown":
				r.State = "unknown"
			case "binding":
				r.BindingHash = strings.Repeat("b", 64)
			case "no-evidence":
				r.ExpiryEvidence = nil
			case "malformed-expiry":
				r.Expires = "not-a-date"
			case "changed-expiry":
				r.Expires = time.Now().Add(-time.Hour).Format(time.RFC3339Nano)
			}
			if mode != "future" {
				reading.Wall = reading.Wall.Add(time.Hour)
				reading.BootNanos += int64(time.Hour)
			}
			if err := i.save(); err != nil {
				t.Fatal(err)
			}
			j, err := newIssuer(i.dir.Name(), i.configPath)
			if err != nil {
				t.Fatal(err)
			}
			defer j.dir.Close()
			j.api = i.api
			j.clock = i.clock
			result, err := j.cleanup(context.Background(), "", true)
			if err != nil || result.PendingCleanup != 1 {
				t.Fatalf("untrusted expiry erased: %s count=%d err=%v", mode, result.PendingCleanup, err)
			}
		})
	}
}

func TestEnrollmentExpiryRequiresMonotonicWaitAndRearmsAfterBoot(t *testing.T) {
	for _, mode := range []string{"wall-forward", "wall-backward", "reboot", "boottime-backward"} {
		t.Run(mode, func(t *testing.T) {
			i, s, cmd, c := issuerFixture(t)
			initial := enrollmentClockReading{Wall: time.Now(), BootID: "boot-one", BootNanos: int64(time.Hour)}
			reading := initial
			i.clock = func() (enrollmentClockReading, error) { return reading, nil }
			fixtureAPI(t, i, c, 300*time.Second, true)
			if _, err := i.issue(context.Background(), cmd, s); err != nil {
				t.Fatal(err)
			}
			reading.Wall = initial.Wall.Add(time.Hour)
			reading.BootNanos += int64(time.Hour)
			switch mode {
			case "wall-forward":
				reading.BootNanos = initial.BootNanos
			case "wall-backward":
				reading.Wall = initial.Wall.Add(-time.Hour)
			case "reboot":
				reading.BootID = "boot-two"
				reading.BootNanos = int64(time.Second)
			case "boottime-backward":
				reading.BootNanos = initial.BootNanos - 1
			}
			r, err := i.cleanup(context.Background(), "", true)
			if err != nil || r.PendingCleanup != 1 {
				t.Fatalf("uncertain clock retired record: %s", mode)
			}
			reading.Wall = initial.Wall.Add(2 * time.Hour)
			if mode == "reboot" {
				reading.BootNanos += int64(306 * time.Second)
			} else {
				reading.BootNanos = initial.BootNanos + int64(2*time.Hour)
			}
			r, err = i.cleanup(context.Background(), "", true)
			if err != nil || r.PendingCleanup != 0 || !strings.Contains(r.Detail, "not confirmed DELETE") {
				t.Fatalf("trusted expiration not retired honestly: %+v %v", r, err)
			}
		})
	}
}

func TestEnrollmentLegacyExpiryMigrationNeverInventsEvidence(t *testing.T) {
	i, _, _, _ := issuerFixture(t)
	row := map[string]any{"enrollmentId": "legacy", "bindingHash": strings.Repeat("a", 64), "keyId": "key", "expires": "2020-01-01T00:00:00Z", "state": "cleanup", "deadline": 1}
	b, _ := json.Marshal(map[string]any{"schemaVersion": 1, "records": []any{row}})
	if err := os.WriteFile(filepath.Join(i.dir.Name(), "enrollment-ledger.json"), b, 0600); err != nil {
		t.Fatal(err)
	}
	j, err := newIssuer(i.dir.Name(), i.configPath)
	if err != nil {
		t.Fatal(err)
	}
	defer j.dir.Close()
	if j.ledger.SchemaVersion != 2 || j.ledger.Records[0].ExpiryEvidence != nil {
		t.Fatal("legacy evidence fabricated")
	}
	r, err := j.cleanup(context.Background(), "", true)
	if err != nil || r.PendingCleanup != 1 {
		t.Fatal("legacy record silently removed")
	}
}
