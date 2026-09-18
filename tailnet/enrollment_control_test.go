package main

import (
	"context"
	"encoding/json"
	"fmt"
	"net"
	"testing"
	"time"
)

func enrollmentControl(t *testing.T, s *service, c enrollmentCommand) enrollmentResponse {
	t.Helper()
	server, client := net.Pipe()
	done := make(chan struct{})
	go func() { handleControl(context.Background(), server, s); close(done) }()
	defer client.Close()
	if err := json.NewEncoder(client).Encode(enrollmentRequest{Version: 2, Command: c}); err != nil {
		t.Fatal(err)
	}
	var response enrollmentResponse
	if err := json.NewDecoder(client).Decode(&response); err != nil {
		t.Fatal(err)
	}
	<-done
	return response
}

func TestEnrollmentCancellationFencesAreBoundedAndClockSafe(t *testing.T) {
	i, _, _, _ := issuerFixture(t)
	reading := enrollmentClockReading{Wall: time.Now(), BootID: "boot", BootNanos: int64(time.Hour)}
	i.clock = func() (enrollmentClockReading, error) { return reading, nil }
	for n := 0; n < 128; n++ {
		if _, err := i.cleanup(context.Background(), fmt.Sprintf("cancel-%d", n), false); err != nil {
			t.Fatal(err)
		}
	}
	if _, err := i.cleanup(context.Background(), "overflow", false); err == nil {
		t.Fatal("unbounded tombstones")
	}
	reading.Wall = reading.Wall.Add(time.Hour)
	if _, err := i.cleanup(context.Background(), "overflow", false); err == nil {
		t.Fatal("forward wall clock erased admission fences")
	}
	reading.BootNanos += int64(11 * time.Second)
	if _, err := i.cleanup(context.Background(), "new-window", false); err != nil {
		t.Fatal(err)
	}
	if len(i.ledger.Fences) != 1 {
		t.Fatal("bounded window did not release capacity")
	}
}

func TestEnrollmentControlCancelBeforeCreateIsDurable(t *testing.T) {
	i, s, cmd, c := issuerFixture(t)
	s.issuer = i
	posts := fixtureAPI(t, i, c, 300*time.Second, false)
	cancel := cmd
	cancel.Action = "cancel"
	cancel.Generation = "cancel-generation"
	r := enrollmentControl(t, s, cancel)
	if r.Error != nil || r.Result == nil {
		t.Fatal("cancel failed")
	}
	j, err := newIssuer(i.dir.Name(), i.configPath)
	if err != nil {
		t.Fatal(err)
	}
	defer j.dir.Close()
	j.api = i.api
	s.issuer = j
	r = enrollmentControl(t, s, cmd)
	if r.Error == nil || *posts != 0 {
		t.Fatalf("cancelled attempt issued: posts=%d", *posts)
	}
}

func TestEnrollmentControlCleanupDoesNotAllowReplay(t *testing.T) {
	i, s, cmd, c := issuerFixture(t)
	s.issuer = i
	posts := fixtureAPI(t, i, c, 300*time.Second, false)
	if r := enrollmentControl(t, s, cmd); r.Error != nil {
		t.Fatal(*r.Error)
	}
	cancel := cmd
	cancel.Action = "cancel"
	if r := enrollmentControl(t, s, cancel); r.Error != nil {
		t.Fatal(*r.Error)
	}
	if r := enrollmentControl(t, s, cmd); r.Error == nil || *posts != 1 {
		t.Fatalf("cleaned attempt issued again: posts=%d", *posts)
	}
}
