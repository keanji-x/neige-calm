package main

import (
	"errors"
	"os"
	"strings"
	"time"

	"golang.org/x/sys/unix"
)

type enrollmentClockReading struct {
	Wall      time.Time
	BootID    string
	BootNanos int64
}

func enrollmentClock() (enrollmentClockReading, error) {
	boot, err := os.ReadFile("/proc/sys/kernel/random/boot_id")
	if err != nil {
		return enrollmentClockReading{}, err
	}
	id := strings.TrimSpace(string(boot))
	var elapsed unix.Timespec
	if !safeID(id) || unix.ClockGettime(unix.CLOCK_BOOTTIME, &elapsed) != nil {
		return enrollmentClockReading{}, errors.New("boot clock unavailable")
	}
	return enrollmentClockReading{Wall: time.Now(), BootID: id, BootNanos: elapsed.Nano()}, nil
}

// Receipt-time evidence uses the actual provider lifetime, not the invitation
// deadline. Waiting a full lifetime on CLOCK_BOOTTIME also tolerates a forward
// wall-clock adjustment. A reboot starts a fresh conservative waiting window.
type expiryEvidence struct {
	Created    string `json:"created"`
	Expires    string `json:"expires"`
	ReceivedAt int64  `json:"receivedAt"`
	BootID     string `json:"bootId"`
	BootNanos  int64  `json:"bootNanos"`
}

func (r *cleanupRecord) expired(reading enrollmentClockReading) bool {
	p := r.ExpiryEvidence
	if p == nil || r.Expires != p.Expires || p.ReceivedAt <= 0 || !safeID(p.BootID) || p.BootNanos < 0 || reading.BootNanos < 0 || !safeID(reading.BootID) {
		return false
	}
	created, e1 := time.Parse(time.RFC3339Nano, p.Created)
	expires, e2 := time.Parse(time.RFC3339Nano, p.Expires)
	lifetime := expires.Sub(created)
	if e1 != nil || e2 != nil || created.IsZero() || lifetime <= 0 || lifetime > 300*time.Second || p.ReceivedAt < created.Add(-5*time.Second).UnixMilli() || p.ReceivedAt >= expires.UnixMilli() || reading.Wall.UnixMilli() < p.ReceivedAt {
		return false
	}
	if p.BootID != reading.BootID {
		p.BootID = reading.BootID
		p.BootNanos = reading.BootNanos
		return false
	}
	return reading.BootNanos >= p.BootNanos && reading.BootNanos-p.BootNanos >= int64(lifetime+5*time.Second) && !reading.Wall.Before(expires.Add(5*time.Second))
}

type enrollmentFence struct {
	EnrollmentID string `json:"enrollmentId"`
	Until        int64  `json:"until"`
	BootID       string `json:"bootId"`
	BootNanos    int64  `json:"bootNanos"`
}

func (i *issuer) fenceAttempt(id string, cancel bool) error {
	reading, err := i.clock()
	if err != nil {
		return errors.New("enrollment admission clock unavailable")
	}
	remaining := make([]enrollmentFence, 0, len(i.ledger.Fences))
	for _, f := range i.ledger.Fences {
		if f.BootID != reading.BootID {
			f.BootID = reading.BootID
			f.BootNanos = reading.BootNanos
		}
		elapsed := reading.BootNanos >= f.BootNanos && reading.BootNanos-f.BootNanos >= int64(10*time.Second)
		if reading.Wall.UnixMilli() >= f.Until && elapsed {
			continue
		}
		remaining = append(remaining, f)
	}
	i.ledger.Fences = remaining
	for n := range i.ledger.Fences {
		f := &i.ledger.Fences[n]
		if f.EnrollmentID != id {
			continue
		}
		if !cancel {
			return errors.New("enrollment attempt already admitted or cancelled; do not replay")
		}
		f.Until = reading.Wall.Add(10 * time.Second).UnixMilli()
		f.BootNanos = reading.BootNanos
		f.BootID = reading.BootID
		return i.save()
	}
	if len(i.ledger.Fences) >= 128 {
		return errors.New("enrollment admission limit reached; wait for pending command deadlines")
	}
	i.ledger.Fences = append(i.ledger.Fences, enrollmentFence{EnrollmentID: id, Until: reading.Wall.Add(10 * time.Second).UnixMilli(), BootID: reading.BootID, BootNanos: reading.BootNanos})
	return i.save()
}
