package main

import (
	"context"
	"errors"
	"net"
	"net/http"
	"path/filepath"
	"testing"
	"time"

	"tailscale.com/ipn/ipnstate"
)

type fakeNode struct {
	state       *ipnstate.Status
	certError   error
	logins      int
	listenBlock chan struct{}
}

func (n *fakeNode) status(context.Context) (*ipnstate.Status, error) { return n.state, nil }
func (n *fakeNode) login(context.Context) error                      { n.logins++; return nil }
func (n *fakeNode) logout(context.Context) error                     { return nil }
func (n *fakeNode) certificate(context.Context, string) error        { return n.certError }
func (n *fakeNode) listen() (net.Listener, error)                    { return net.Listen("tcp", "127.0.0.1:0") }

func TestNeedsLoginAndApprovalAreRunningStates(t *testing.T) {
	target := filepath.Join(t.TempDir(), "ingress.sock")
	n := &fakeNode{state: &ipnstate.Status{BackendState: "NeedsLogin", AuthURL: "https://login.tailscale.com/a/fixture"}}
	s := newService(n, target)
	s.refresh(context.Background())
	if st := s.snapshot(); st.Phase != "needs-login" || !st.ProcessRunning || st.HTTPSReady {
		t.Fatal(st)
	}
	login, err := s.login(context.Background())
	if err != nil || login == nil || n.logins != 1 {
		t.Fatal(login, err)
	}
	n.state.BackendState = "NeedsMachineAuth"
	s.refresh(context.Background())
	if st := s.snapshot(); st.Phase != "needs-approval" || !st.ProcessRunning {
		t.Fatal(st)
	}
	if _, err = s.login(context.Background()); err == nil {
		t.Fatal("approval state silently started another enrollment")
	}
}
func TestHTTPSDoesNotFallBackWhenCertificatesAreUnavailable(t *testing.T) {
	target := unixUpstream(t, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { w.WriteHeader(200) }))
	n := &fakeNode{state: &ipnstate.Status{BackendState: "Running", Self: &ipnstate.PeerStatus{DNSName: "fixture.example.ts.net.", Online: true}, CurrentTailnet: &ipnstate.TailnetStatus{MagicDNSEnabled: true}, CertDomains: []string{"fixture.example.ts.net"}}, certError: errors.New("certificate unavailable")}
	s := newService(n, target)
	defer s.closeIngress()
	s.refresh(context.Background())
	if st := s.snapshot(); st.HTTPSReady || st.Origin != nil || !st.UpstreamReady || st.Phase != "degraded" {
		t.Fatal(st)
	}
	n.certError = nil
	s.refresh(context.Background())
	waitListener(t, s)
	s.refresh(context.Background())
	if st := s.snapshot(); !st.HTTPSReady || st.Origin == nil || *st.Origin != "https://fixture.example.ts.net" || st.Phase != "online" {
		t.Fatal(st)
	}
	n.certError = errors.New("certificate became unavailable")
	s.refresh(context.Background())
	if st := s.snapshot(); st.HTTPSReady || st.Origin != nil || st.Phase != "degraded" {
		t.Fatal(st)
	}
}

func waitListener(t *testing.T, s *service) {
	t.Helper()
	deadline := time.Now().Add(time.Second)
	for time.Now().Before(deadline) {
		s.mu.Lock()
		ready := s.listener != nil
		s.mu.Unlock()
		if ready {
			return
		}
		time.Sleep(time.Millisecond)
	}
	t.Fatal("listener did not start")
}

func TestPendingTLSListenDoesNotFreezeNeedsLoginStatus(t *testing.T) {
	target := unixUpstream(t, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { w.WriteHeader(200) }))
	blocked := make(chan struct{})
	n := &fakeNode{state: &ipnstate.Status{BackendState: "Running", Self: &ipnstate.PeerStatus{DNSName: "fixture.example.ts.net.", Online: true}, CurrentTailnet: &ipnstate.TailnetStatus{MagicDNSEnabled: true}, CertDomains: []string{"fixture.example.ts.net"}}, listenBlock: blocked}
	s := newService(n, target)
	ctx, cancel := context.WithCancel(context.Background())
	defer func() { cancel(); close(blocked) }()
	refreshed := make(chan struct{})
	go func() { s.refresh(ctx); close(refreshed) }()
	select {
	case <-refreshed:
	case <-time.After(time.Second):
		t.Fatal("pending TLS froze node status")
	}
	n.state.BackendState = "NeedsLogin"
	s.refresh(ctx)
	if st := s.snapshot(); st.Phase != "needs-login" || !st.ProcessRunning {
		t.Fatal(st)
	}
}
