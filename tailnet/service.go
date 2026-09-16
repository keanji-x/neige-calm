package main

import (
	"context"
	"errors"
	"net"
	"net/http"
	"net/url"
	"os"
	"strings"
	"sync"
	"time"

	"tailscale.com/ipn/ipnstate"
)

type node interface {
	status(context.Context) (*ipnstate.Status, error)
	login(context.Context) error
	logout(context.Context) error
	certificate(context.Context, string) error
	listen() (net.Listener, error)
}

type service struct {
	node       node
	transport  *http.Transport
	mu         sync.Mutex
	current    status
	generation uint64
	server     *http.Server
	listener   *trackedListener
	listening  bool
}

func newService(n node, target string) *service {
	pid := uint32(os.Getpid())
	return &service{node: n, transport: upstreamTransport(target), current: status{
		DesiredEnabled: true, Phase: "starting", ProcessRunning: true, ChildPID: &pid,
		NodeState: "starting", Addresses: []string{}, Detail: "Starting private Tailnet node",
	}}
}
func (s *service) snapshot() status { s.mu.Lock(); defer s.mu.Unlock(); return s.current }
func (s *service) run(ctx context.Context) {
	for {
		s.refresh(ctx)
		select {
		case <-ctx.Done():
			s.closeIngress()
			s.transport.CloseIdleConnections()
			return
		case <-time.After(2 * time.Second):
		}
	}
}
func (s *service) refresh(ctx context.Context) {
	s.mu.Lock()
	generation := s.generation
	s.mu.Unlock()
	probeCtx, cancel := context.WithTimeout(ctx, 5*time.Second)
	st, err := s.node.status(probeCtx)
	cancel()
	current := s.snapshot()
	current.UpstreamReady = probeUpstream(ctx, s.transport)
	current.Origin = nil
	current.DNSName = nil
	current.NodeID = nil
	current.Addresses = []string{}
	if err != nil {
		current.NodeState = "offline"
		current.Phase = "degraded"
		current.Detail = "Node status unavailable; check network connection"
	} else {
		switch st.BackendState {
		case "NeedsLogin":
			current.NodeState = "needs-login"
			current.Phase = "needs-login"
			current.Detail = "Sign in to Tailscale to connect this private node"
		case "NeedsMachineAuth":
			current.NodeState = "needs-approval"
			current.Phase = "needs-approval"
			current.Detail = "Approve this device in the Tailscale admin console"
		case "Running":
			current.NodeState = "online"
			current.Phase = "degraded"
			current.Detail = "Enable MagicDNS and HTTPS certificates in the Tailscale DNS settings"
		default:
			current.NodeState = "starting"
			current.Phase = "starting"
			current.Detail = "Waiting for the Tailnet connection"
		}
		if st.Self != nil {
			if st.BackendState == "Running" && !st.Self.Online {
				current.NodeState = "offline"
				current.Phase = "degraded"
				current.Detail = "Tailnet control connection is offline; existing peer connectivity may still work"
			}
			name := strings.TrimSuffix(st.Self.DNSName, ".")
			if validDNSName(name) {
				current.DNSName = &name
			}
			id := string(st.Self.ID)
			if id != "" {
				current.NodeID = &id
			}
			for _, ip := range st.TailscaleIPs {
				current.Addresses = append(current.Addresses, ip.String())
			}
		}
	}
	ready := err == nil && st.BackendState == "Running" && current.DNSName != nil && st.CurrentTailnet != nil && st.CurrentTailnet.MagicDNSEnabled && contains(st.CertDomains, *current.DNSName)
	if ready {
		certCtx, cancel := context.WithTimeout(ctx, 5*time.Second)
		err = s.node.certificate(certCtx, *current.DNSName)
		cancel()
		ready = err == nil
		if !ready {
			current.Detail = "HTTPS certificate is unavailable; check Tailscale HTTPS authorization and network connection"
		}
	}
	if !ready {
		s.mu.Lock()
		if generation != s.generation {
			s.mu.Unlock()
			return
		}
		if s.listening {
			s.generation++
			generation = s.generation
		}
		s.closeIngressLocked()
		s.mu.Unlock()
	} else {
		s.mu.Lock()
		needsListener := s.listener == nil && !s.listening && generation == s.generation
		if needsListener {
			s.listening = true
		}
		s.mu.Unlock()
		if needsListener {
			// ListenTLS internally calls Up(context.Background()). Keep one
			// pending call, without freezing status/login on a network change.
			go s.startListener(ctx, generation)
			current.Phase = "starting"
			current.Detail = "Preparing private HTTPS listener"
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if generation != s.generation {
		return
	}
	current.HTTPSReady = s.listener != nil
	if current.HTTPSReady && current.DNSName != nil {
		origin := "https://" + *current.DNSName
		current.Origin = &origin
		if current.UpstreamReady && current.NodeState == "online" {
			current.Phase = "online"
			current.Detail = "Private Tailnet HTTPS is ready"
		} else if !current.UpstreamReady {
			current.Phase = "degraded"
			current.Detail = "Neige is restarting or its restricted ingress is unavailable"
		}
	}
	s.current = current
}
func (s *service) startListener(ctx context.Context, generation uint64) {
	listener, err := s.node.listen()
	s.mu.Lock()
	defer s.mu.Unlock()
	s.listening = false
	if err != nil || generation != s.generation || ctx.Err() != nil {
		if listener != nil {
			listener.Close()
		}
		return
	}
	s.listener = newTrackedListener(listener)
	s.server = &http.Server{Handler: restrictedProxy(s.transport), ReadHeaderTimeout: 10 * time.Second, IdleTimeout: 60 * time.Second, MaxHeaderBytes: 32768}
	server, tracked := s.server, s.listener
	go func() {
		_ = server.Serve(tracked)
		s.mu.Lock()
		defer s.mu.Unlock()
		if s.listener == tracked {
			tracked.closeAll()
			s.listener = nil
			s.server = nil
			s.current.HTTPSReady = false
			s.current.Origin = nil
			s.current.Phase = "degraded"
			s.current.Detail = "HTTPS listener stopped; reconnecting"
		}
	}()
}

func (s *service) closeIngress() {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.closeIngressLocked()
}
func (s *service) closeIngressLocked() {
	if s.listener != nil {
		s.listener.closeAll()
		s.listener = nil
	}
	if s.server != nil {
		s.server.Close()
		s.server = nil
	}
	s.current.HTTPSReady = false
	s.current.Origin = nil
}
func (s *service) login(ctx context.Context) (*string, error) {
	st, err := s.node.status(ctx)
	if err != nil {
		return nil, err
	}
	if st.BackendState != "NeedsLogin" {
		return nil, errors.New("node does not need login")
	}
	s.mu.Lock()
	s.generation++
	s.mu.Unlock()
	s.closeIngress()
	if err = s.node.login(ctx); err != nil {
		return nil, err
	}
	for {
		st, err = s.node.status(ctx)
		if err != nil {
			return nil, err
		}
		if st.AuthURL != "" {
			parsed, e := url.Parse(st.AuthURL)
			if e != nil || parsed.Scheme != "https" || parsed.Host != "login.tailscale.com" || parsed.User != nil {
				return nil, errors.New("invalid login URL")
			}
			return &st.AuthURL, nil
		}
		select {
		case <-ctx.Done():
			return nil, ctx.Err()
		case <-time.After(100 * time.Millisecond):
		}
	}
}
func (s *service) logout(ctx context.Context) error {
	s.mu.Lock()
	s.generation++
	s.mu.Unlock()
	s.closeIngress()
	err := s.node.logout(ctx)
	s.mu.Lock()
	s.current.NodeState = "needs-login"
	s.current.Phase = "needs-login"
	s.current.Detail = "Signed out of Tailnet"
	s.mu.Unlock()
	return err
}
func validDNSName(name string) bool {
	if !strings.HasSuffix(name, ".ts.net") {
		return false
	}
	for _, r := range name {
		if !(r >= 'a' && r <= 'z' || r >= '0' && r <= '9' || r == '.' || r == '-') {
			return false
		}
	}
	return true
}
func contains(values []string, value string) bool {
	for _, v := range values {
		if v == value {
			return true
		}
	}
	return false
}

// Track the real transport through HTTP upgrades, so stopping the node closes
// hijacked WebSockets too. net/http.Server.Close alone does not close them.
type trackedListener struct {
	net.Listener
	mu     sync.Mutex
	conns  map[*trackedConn]struct{}
	closed bool
}
type trackedConn struct {
	net.Conn
	owner *trackedListener
}

func newTrackedListener(l net.Listener) *trackedListener {
	return &trackedListener{Listener: l, conns: map[*trackedConn]struct{}{}}
}
func (l *trackedListener) Accept() (net.Conn, error) {
	c, err := l.Listener.Accept()
	if err != nil {
		return nil, err
	}
	l.mu.Lock()
	defer l.mu.Unlock()
	if l.closed {
		c.Close()
		return nil, net.ErrClosed
	}
	wrapped := &trackedConn{Conn: c, owner: l}
	l.conns[wrapped] = struct{}{}
	return wrapped, nil
}
func (c *trackedConn) Close() error {
	c.owner.mu.Lock()
	delete(c.owner.conns, c)
	c.owner.mu.Unlock()
	return c.Conn.Close()
}
func (l *trackedListener) closeAll() {
	l.Listener.Close()
	l.mu.Lock()
	l.closed = true
	for c := range l.conns {
		c.Conn.Close()
	}
	l.conns = map[*trackedConn]struct{}{}
	l.mu.Unlock()
}
