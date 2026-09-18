package main

import (
	"context"
	"errors"
	"io"
	"net"
	"net/http"
	"sync"
	"time"
)

// A proxy port belongs to one document generation and one immutable peer
// binding. A retired connection can never be redirected to a replacement.
type tailnetTunnel struct {
	engine        *engine
	binding       tailnetBinding
	authority     string
	listener      net.Listener
	server        *http.Server
	ctx           context.Context
	cancel        context.CancelFunc
	networkCtx    context.Context
	networkCancel context.CancelFunc
	mu            sync.Mutex
	closed        bool
	connections   map[net.Conn]bool
}

func (e *engine) closeTunnel() {
	e.enrollmentMu.Lock()
	defer e.enrollmentMu.Unlock()
	e.closeTunnelLocked()
}
func (e *engine) closeTunnelLocked() {
	if e.tunnel != nil {
		e.tunnel.close()
		e.tunnel = nil
	}
}
func (t *tailnetTunnel) close() {
	t.mu.Lock()
	t.closed = true
	t.cancel()
	for connection := range t.connections {
		connection.Close()
	}
	t.mu.Unlock()
	t.listener.Close()
	t.server.Close()
}

// A transient loss closes sockets and in-flight dials, retaining only the
// closed-over binding and port so the same document can retry after recovery.
func (t *tailnetTunnel) invalidateNetwork() {
	t.mu.Lock()
	defer t.mu.Unlock()
	if t.closed {
		return
	}
	t.networkCancel()
	for connection := range t.connections {
		connection.Close()
	}
	t.networkCtx, t.networkCancel = context.WithCancel(t.ctx)
}
func (t *tailnetTunnel) networkContext() context.Context {
	t.mu.Lock()
	defer t.mu.Unlock()
	return t.networkCtx
}
func (t *tailnetTunnel) track(connection net.Conn) bool { return t.trackGeneration(connection, t.ctx) }
func (t *tailnetTunnel) trackGeneration(connection net.Conn, admitted context.Context) bool {
	t.mu.Lock()
	defer t.mu.Unlock()
	if t.closed || admitted.Err() != nil {
		connection.Close()
		return false
	}
	t.connections[connection] = true
	return true
}
func (t *tailnetTunnel) forget(connection net.Conn) {
	t.mu.Lock()
	delete(t.connections, connection)
	t.mu.Unlock()
	connection.Close()
}
func (e *engine) installTunnelLocked(binding tailnetBinding) (*tailnetTunnel, error) {
	e.closeTunnelLocked()
	parsed, err := parseTailnetOrigin(binding.Origin)
	if err != nil {
		return nil, err
	}
	listener, err := net.Listen("tcp4", "127.0.0.1:0")
	if err != nil {
		return nil, err
	}
	ctx, cancel := context.WithCancel(context.Background())
	tunnel := &tailnetTunnel{engine: e, binding: binding, authority: net.JoinHostPort(parsed.hostname, parsedPort(parsed)), listener: listener, ctx: ctx, cancel: cancel, connections: make(map[net.Conn]bool)}
	tunnel.networkCtx, tunnel.networkCancel = context.WithCancel(ctx)
	tunnel.server = &http.Server{Handler: tunnel, ReadHeaderTimeout: 5 * time.Second, MaxHeaderBytes: 8192, ConnState: func(connection net.Conn, state http.ConnState) {
		if state == http.StateNew {
			tunnel.track(connection)
		}
		if state == http.StateClosed {
			tunnel.forget(connection)
		}
	}}
	e.tunnel = tunnel
	go tunnel.server.Serve(listener)
	return tunnel, nil
}
func (t *tailnetTunnel) proxyURL() string { return "http://" + t.listener.Addr().String() }
func (t *tailnetTunnel) dial(ctx context.Context, network, address string) (net.Conn, error) {
	if network != "tcp" || address != t.authority || t.ctx.Err() != nil {
		return nil, errors.New("只允许连接已指定的工作区")
	}
	runtime, err := t.engine.runtime()
	if err != nil {
		return nil, err
	}
	status, err := runtime.Status(ctx)
	if err != nil {
		return nil, errors.New("无法读取可信节点状态")
	}
	destination, err := validateTailnetTarget(t.binding, status)
	if err != nil {
		t.invalidateNetwork()
		return nil, err
	}
	connection, err := runtime.Dial(ctx, "tcp", destination.Address.String())
	if err != nil {
		return nil, err
	}
	if ctx.Err() != nil {
		connection.Close()
		return nil, ctx.Err()
	}
	if !t.trackGeneration(connection, ctx) {
		return nil, net.ErrClosed
	}
	return connection, nil
}
func (t *tailnetTunnel) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodConnect || r.Host != t.authority || r.URL.Host != t.authority {
		http.Error(w, "Target denied", http.StatusForbidden)
		return
	}
	ctx, cancel := context.WithTimeout(t.networkContext(), 15*time.Second)
	defer cancel()
	upstream, err := t.dial(ctx, "tcp", r.Host)
	if err != nil {
		http.Error(w, "Workspace connection unavailable", http.StatusBadGateway)
		return
	}
	defer t.forget(upstream)
	local, buffer, err := w.(http.Hijacker).Hijack()
	if err != nil {
		return
	}
	if !t.track(local) {
		return
	}
	defer t.forget(local)
	if _, err = buffer.WriteString("HTTP/1.1 200 Connection Established\r\n\r\n"); err != nil {
		return
	}
	if err = buffer.Flush(); err != nil {
		return
	}
	done := make(chan struct{})
	go func() { io.Copy(upstream, buffer); upstream.Close(); local.Close(); close(done) }()
	io.Copy(local, upstream)
	local.Close()
	upstream.Close()
	<-done
}
