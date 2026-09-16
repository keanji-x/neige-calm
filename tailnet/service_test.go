package main

import (
	"context"
	"errors"
	"net"
	"net/http"
	"path/filepath"
	"sync"
	"testing"
	"time"

	"tailscale.com/ipn/ipnstate"
)

type fakeNode struct {
	state         *ipnstate.Status
	certError     error
	logins        int
	listenBlock   chan struct{}
	listenEntered chan struct{}
	listenClosed  chan struct{}
}

func (n *fakeNode) status(context.Context) (*ipnstate.Status, error) { return n.state, nil }
func (n *fakeNode) login(context.Context) error                      { n.logins++; return nil }
func (n *fakeNode) logout(context.Context) error                     { return nil }
func (n *fakeNode) certificate(context.Context, string) error        { return n.certError }
func (n *fakeNode) listen() (net.Listener, error) {
	if n.listenEntered != nil {
		close(n.listenEntered)
	}
	if n.listenBlock != nil {
		<-n.listenBlock
	}
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil || n.listenClosed == nil {
		return listener, err
	}
	return &observedListener{Listener: listener, closed: n.listenClosed}, nil
}

type observedListener struct {
	net.Listener
	closed chan struct{}
	once   sync.Once
}

func (l *observedListener) Close() error {
	l.once.Do(func() { close(l.closed) })
	return l.Listener.Close()
}

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
	for _, change := range []string{"needs-login", "canceled"} {
		t.Run(change, func(t *testing.T) {
			target := unixUpstream(t, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { w.WriteHeader(200) }))
			blocked, entered, closed := make(chan struct{}), make(chan struct{}), make(chan struct{})
			n := &fakeNode{state: &ipnstate.Status{BackendState: "Running", Self: &ipnstate.PeerStatus{DNSName: "fixture.example.ts.net.", Online: true}, CurrentTailnet: &ipnstate.TailnetStatus{MagicDNSEnabled: true}, CertDomains: []string{"fixture.example.ts.net"}}, listenBlock: blocked, listenEntered: entered, listenClosed: closed}
			s := newService(n, target)
			ctx, cancel := context.WithCancel(context.Background())
			var release sync.Once
			defer func() { cancel(); release.Do(func() { close(blocked) }); s.closeIngress() }()
			refreshed := make(chan struct{})
			go func() { s.refresh(ctx); close(refreshed) }()
			select {
			case <-entered:
			case <-time.After(time.Second):
				t.Fatal("listen operation did not enter barrier")
			}
			select {
			case <-refreshed:
			case <-time.After(time.Second):
				t.Fatal("blocked TLS listen froze refresh")
			}
			if change == "needs-login" {
				n.state.BackendState = "NeedsLogin"
				s.refresh(ctx)
				if st := s.snapshot(); st.Phase != "needs-login" || !st.ProcessRunning {
					t.Fatal(st)
				}
			} else {
				cancel()
			}
			// The operation really is still blocked: it cannot have returned
			// or closed its listener before this explicit release.
			select {
			case <-closed:
				t.Fatal("listener escaped the blocked operation")
			default:
			}
			release.Do(func() { close(blocked) })
			select {
			case <-closed:
			case <-time.After(time.Second):
				t.Fatal("late listener reopened after its generation was retired")
			}
			s.mu.Lock()
			active := s.listener != nil
			s.mu.Unlock()
			if active {
				t.Fatal("retired listener was published")
			}
		})
	}
}
