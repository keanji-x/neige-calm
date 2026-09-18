package main

import (
	"testing"
	"time"
)

func testOperationPermit(t *testing.T) *operationPermit {
	t.Helper()
	admission := &operationAdmission{}
	token, err := admission.reserve()
	if err != nil {
		t.Fatal(err)
	}
	permit, err := admission.claim(token)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = permit.revoke() })
	return permit
}

func TestCancellationBeforeJNIAdmissionRejectsDelayedEnrollment(t *testing.T) {
	e, node, qr := enrollmentFixture(t, false)
	admission := &operationAdmission{}
	token, err := admission.reserve()
	if err != nil {
		t.Fatal(err)
	}
	// Cover cancellation both before claim and after claim but before engine
	// admission; neither may mint another permit for the old worker.
	permit, err := admission.claim(token)
	if err != nil {
		t.Fatal(err)
	}
	// Java has checked its cancellation flag but has not entered the JNI call.
	// A settings save now supersedes that job. Its late native admission must
	// not turn the already-cancelled user intent into a new generation.
	if err := admission.revoke(token); err != nil {
		t.Fatal(err)
	}
	if _, err := e.enroll(qr, permit); err == nil || node.authCalls != 0 {
		t.Fatal("cancelled worker entered JNI and registered after supersession")
	}
	if _, err := admission.claim(token); err == nil {
		t.Fatal("cancelled token admitted again")
	}
}

func TestNativeAdmissionIsOneShotAndOldCancellationCannotCancelReplacement(t *testing.T) {
	a := &operationAdmission{}
	old, err := a.reserve()
	if err != nil {
		t.Fatal(err)
	}
	if err := a.revoke(old); err != nil {
		t.Fatal(err)
	}
	if _, err := a.claim(old); err == nil {
		t.Fatal("revoked token admitted before JNI")
	}
	current, err := a.reserve()
	if err != nil {
		t.Fatal(err)
	}
	p, err := a.claim(current)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := a.claim(current); err == nil {
		t.Fatal("token reused")
	}
	if err := a.revoke(old); err != nil {
		t.Fatal(err)
	}
	if p.ctx.Err() != nil {
		t.Fatal("old cancellation revoked new operation")
	}
	if err := a.revokeAll(); err != nil {
		t.Fatal(err)
	}
	if p.ctx.Err() == nil {
		t.Fatal("lifecycle cancellation did not revoke permit")
	}
}

func TestNativeSupersessionDuringTLSCannotSaveTargetOrInstallTunnel(t *testing.T) {
	e, node, qr := enrollmentFixture(t, true)
	a := &operationAdmission{}
	token, err := a.reserve()
	if err != nil {
		t.Fatal(err)
	}
	p, err := a.claim(token)
	if err != nil {
		t.Fatal(err)
	}
	node.dialEntered, node.dialRelease = make(chan struct{}), make(chan struct{})
	done := make(chan error, 1)
	go func() { _, err := e.enroll(qr, p); done <- err }()
	select {
	case <-node.dialEntered:
	case <-time.After(time.Second):
		t.Fatal("TLS stage not entered")
	}
	// The same revocation used by Pending.cancel for save/select/pause/timeout.
	if err := a.revoke(token); err != nil {
		t.Fatal(err)
	}
	close(node.dialRelease)
	select {
	case err := <-done:
		if err == nil {
			t.Fatal("superseded enrollment completed")
		}
	case <-time.After(time.Second):
		t.Fatal("operation did not cancel")
	}
	if _, err := savedTarget(e.dir, "https://alpha.tail.example:10000"); err == nil {
		t.Fatal("stale target saved")
	}
	if e.tunnel != nil {
		t.Fatal("stale tunnel installed")
	}
}

func TestCanceledResetPermitCannotLogoutOrInvalidateSavedTarget(t *testing.T) {
	e, node, qr := enrollmentFixture(t, true)
	if _, err := e.enroll(qr, testOperationPermit(t)); err != nil {
		t.Fatal(err)
	}
	p := testOperationPermit(t)
	if err := p.revoke(); err != nil {
		t.Fatal(err)
	}
	if err := e.resetEnrollment(p); err == nil || node.logoutCalls != 0 {
		t.Fatal("late reset logged out native identity")
	}
	if _, err := savedTarget(e.dir, "https://alpha.tail.example:10000"); err != nil {
		t.Fatal("late reset cleared saved target")
	}
}

func TestDelayedOldAdmissionCannotSupersedeNewScanOrReset(t *testing.T) {
	for _, kind := range []string{"scan", "reset"} {
		t.Run(kind, func(t *testing.T) {
			e, node, qr := enrollmentFixture(t, true)
			a := &operationAdmission{}
			oldToken, err := a.reserve()
			if err != nil {
				t.Fatal(err)
			}
			old, err := a.claim(oldToken)
			if err != nil {
				t.Fatal(err)
			}
			newToken, err := a.reserve()
			if err != nil {
				t.Fatal(err)
			}
			newPermit, err := a.claim(newToken)
			if err != nil {
				t.Fatal(err)
			}
			if kind == "scan" {
				if _, err := e.enroll(qr, newPermit); err != nil {
					t.Fatal(err)
				}
			} else {
				if err := e.resetEnrollment(newPermit); err != nil {
					t.Fatal(err)
				}
			}
			generation := e.enrollmentGeneration
			tunnel := e.tunnel
			if _, err := e.enroll(qr, old); err == nil {
				t.Fatal("old JNI admission replaced current intent")
			}
			if e.enrollmentGeneration != generation || e.tunnel != tunnel || node.authCalls != 0 {
				t.Fatal("old worker changed current native state")
			}
		})
	}
}
