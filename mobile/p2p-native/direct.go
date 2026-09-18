package main

import (
	"context"
	"crypto/tls"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httputil"
	"net/netip"
	"net/url"
	"strconv"
	"strings"
	"sync"
	"time"
)

var directState struct {
	sync.Mutex
	current *directProxy
}

type directProxy struct {
	target     *url.URL
	server     *http.Server
	listener   net.Listener
	reverse    *httputil.ReverseProxy
	mu         sync.Mutex
	closed     bool
	clients    map[net.Conn]bool
	ctx        context.Context
	cancel     context.CancelFunc
	binding    directBinding
	bindingRaw string
	bindingErr error
	network    directNetwork
}
type trackedListener struct {
	net.Listener
	owner *directProxy
}
type trackedConnection struct {
	net.Conn
	owner *directProxy
}

func (c *trackedConnection) Close() error {
	c.owner.mu.Lock()
	delete(c.owner.clients, c)
	c.owner.mu.Unlock()
	return c.Conn.Close()
}
func (l trackedListener) Accept() (net.Conn, error) {
	c, err := l.Listener.Accept()
	if err != nil {
		return nil, err
	}
	wrapped := &trackedConnection{Conn: c, owner: l.owner}
	l.owner.mu.Lock()
	defer l.owner.mu.Unlock()
	if l.owner.closed {
		c.Close()
		return nil, net.ErrClosed
	}
	l.owner.clients[wrapped] = true
	return wrapped, nil
}
func directOrigin(raw string) (*url.URL, error) {
	u, err := url.Parse(raw)
	if err != nil {
		return nil, fmt.Errorf("无效的服务器地址")
	}
	if u.User != nil || (u.Scheme != "http" && u.Scheme != "https") || u.Hostname() == "" || u.Path != "" || u.RawQuery != "" || u.Fragment != "" {
		return nil, fmt.Errorf("无效的服务器地址")
	}
	host := strings.ToLower(u.Hostname())
	if host == "localhost" || strings.HasSuffix(host, ".localhost") || strings.Contains(host, "%") {
		return nil, fmt.Errorf("不能连接 App 自身或本机保留地址")
	}
	ip, ipErr := netip.ParseAddr(host)
	if u.Scheme == "http" && ipErr != nil {
		return nil, fmt.Errorf("HTTP 连接需要填写明确的 IP 地址")
	}
	if ipErr == nil && !allowedDirectAddress(ip) {
		return nil, fmt.Errorf("不能连接保留地址")
	}
	if u.Port() != "" {
		port, err := strconv.ParseUint(u.Port(), 10, 16)
		if err != nil || port == 0 {
			return nil, fmt.Errorf("无效的服务器端口")
		}
	}
	return u, nil
}
func (p *directProxy) close() {
	p.mu.Lock()
	p.closed = true
	p.cancel()
	clients := make([]net.Conn, 0, len(p.clients))
	for c := range p.clients {
		clients = append(clients, c)
	}
	p.mu.Unlock()
	if p.listener != nil {
		p.listener.Close()
	}
	if p.server != nil {
		p.server.Close()
	}
	for _, c := range clients {
		c.Close()
	}
	if transport, ok := p.reverse.Transport.(*http.Transport); ok {
		transport.CloseIdleConnections()
	}
}
func stopDirect() {
	directState.Lock()
	defer directState.Unlock()
	if directState.current != nil {
		directState.current.close()
		directState.current = nil
	}
}
func directForwarder(target *url.URL) *httputil.ReverseProxy {
	return &httputil.ReverseProxy{Rewrite: func(request *httputil.ProxyRequest) {
		request.SetURL(target)
		request.Out.Host = target.Host
		// This is an in-app transport, not a trusted server-side ingress proxy.
		// Do not claim that the phone's loopback socket is the upstream client IP.
		for _, header := range []string{"Forwarded", "X-Forwarded-For", "X-Forwarded-Host", "X-Forwarded-Proto", "X-Real-IP"} {
			request.Out.Header.Del(header)
		}
	}}
}

func newDirectProxy(target *url.URL, binding string, network directNetwork) *directProxy {
	ctx, cancel := context.WithCancel(context.Background())
	p := &directProxy{target: target, clients: make(map[net.Conn]bool), ctx: ctx, cancel: cancel, bindingRaw: binding, network: network}
	p.binding, p.bindingErr = parseDirectBinding(target, binding)
	p.reverse = directForwarder(target)
	p.reverse.Transport = &http.Transport{Proxy: nil, DialContext: p.dial,
		TLSClientConfig:       &tls.Config{ServerName: target.Hostname(), RootCAs: network.roots, MinVersion: tls.VersionTLS12},
		ResponseHeaderTimeout: 15 * time.Second, ForceAttemptHTTP2: true}
	return p
}

// Installation never resolves DNS: cold bundled assets paint before requests
// validate the persisted binding. A missing hostname binding denies networking.
func configureDirect(raw, binding string) string {
	target, err := directOrigin(raw)
	if err != nil {
		return failure(err)
	}
	directState.Lock()
	defer directState.Unlock()
	if old := directState.current; old != nil {
		if old.target.String() == target.String() && old.bindingRaw == binding {
			return encoded(map[string]any{"ok": true, "proxy": "http://" + old.listener.Addr().String()})
		}
		old.close()
		directState.current = nil
	}
	listener, err := net.Listen("tcp4", "127.0.0.1:0")
	if err != nil {
		return failure(err)
	}
	p := newDirectProxy(target, binding, systemDirectNetwork())
	p.listener = listener
	p.server = &http.Server{Handler: p, ReadHeaderTimeout: 5 * time.Second, MaxHeaderBytes: 65536}
	directState.current = p
	go p.server.Serve(trackedListener{Listener: listener, owner: p})
	return encoded(map[string]any{"ok": true, "proxy": "http://" + listener.Addr().String()})
}

type directDestinationKey struct{}
type directDestination struct {
	owner   *directProxy
	address string
}

func (p *directProxy) authorize(ctx context.Context) (context.Context, error) {
	if err := p.ctx.Err(); err != nil {
		return ctx, err
	}
	if p.bindingErr != nil {
		return ctx, p.bindingErr
	}
	lookupContext, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	address, err := validateDirectBinding(lookupContext, p.target, p.binding, p.network)
	if err != nil {
		return ctx, err
	}
	return context.WithValue(ctx, directDestinationKey{}, directDestination{p, address}), nil
}

func (p *directProxy) dial(ctx context.Context, network, requested string) (net.Conn, error) {
	destination, ok := ctx.Value(directDestinationKey{}).(directDestination)
	if !ok || destination.owner != p || network != "tcp" || requested != authority(p.target) || p.ctx.Err() != nil {
		return nil, fmt.Errorf("Unconfigured target")
	}
	// Numeric-only dial of this request's validated answer. Never resolve again.
	connection, err := p.network.dial(ctx, network, destination.address)
	if err != nil {
		return nil, err
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.closed || ctx.Err() != nil {
		connection.Close()
		return nil, net.ErrClosed
	}
	tracked := &trackedConnection{Conn: connection, owner: p}
	p.clients[tracked] = true
	return tracked, nil
}
func authority(u *url.URL) string {
	port := u.Port()
	if port == "" {
		if u.Scheme == "https" {
			port = "443"
		} else {
			port = "80"
		}
	}
	return net.JoinHostPort(strings.ToLower(u.Hostname()), port)
}
func (p *directProxy) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if r.Method == http.MethodConnect {
		if r.Host != authority(p.target) || r.URL.Host != r.Host {
			http.Error(w, "Unconfigured target", 403)
			return
		}
		ctx, cancel := context.WithTimeout(r.Context(), 5*time.Second)
		defer cancel()
		stop := context.AfterFunc(p.ctx, cancel)
		defer stop()
		ctx, err := p.authorize(ctx)
		if err != nil {
			http.Error(w, "Connection unavailable", 502)
			return
		}
		upstream, err := p.dial(ctx, "tcp", authority(p.target))
		if err != nil {
			http.Error(w, "Connection unavailable", 502)
			return
		}
		defer upstream.Close()
		local, buffer, err := w.(http.Hijacker).Hijack()
		if err != nil {
			return
		}
		defer local.Close()
		buffer.WriteString("HTTP/1.1 200 Connection Established\r\n\r\n")
		if err = buffer.Flush(); err != nil {
			return
		}
		done := make(chan struct{})
		go func() { io.Copy(upstream, buffer); local.Close(); upstream.Close(); close(done) }()
		io.Copy(local, upstream)
		local.Close()
		upstream.Close()
		<-done
		return
	}
	// Every redirect is a new proxy request. Unlike WebView interception, this
	// check sees redirect hops too, before dialing or forwarding any credentials.
	if r.URL.User != nil || r.URL.Scheme != p.target.Scheme || authority(r.URL) != authority(p.target) || r.Host != r.URL.Host {
		http.Error(w, "Unconfigured target", 403)
		return
	}
	ctx, cancel := context.WithCancel(r.Context())
	defer cancel()
	stop := context.AfterFunc(p.ctx, cancel)
	defer stop()
	ctx, err := p.authorize(ctx)
	if err != nil {
		http.Error(w, "Connection unavailable", 502)
		return
	}
	p.reverse.ServeHTTP(w, r.WithContext(ctx))
}
