package main

import (
	"context"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httputil"
	"net/url"
	"strings"
	"sync"
	"time"
)

var directState struct {
	sync.Mutex
	current *directProxy
}

type directProxy struct {
	target   *url.URL
	server   *http.Server
	listener net.Listener
	reverse  *httputil.ReverseProxy
	mu       sync.Mutex
	closed   bool
	clients  map[net.Conn]bool
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
	ip := net.ParseIP(host)
	if u.Scheme == "http" && ip == nil {
		return nil, fmt.Errorf("HTTP 连接需要填写明确的 IP 地址")
	}
	if ip != nil {
		if ip.IsLoopback() || ip.IsUnspecified() || ip.IsLinkLocalUnicast() || ip.IsMulticast() {
			return nil, fmt.Errorf("不能连接保留地址")
		}
		if v4 := ip.To4(); v4 != nil && (v4[0] == 0 || v4[0] >= 224) {
			return nil, fmt.Errorf("不能连接保留地址")
		}
	}
	return u, nil
}
func (p *directProxy) close() {
	p.mu.Lock()
	p.closed = true
	clients := make([]net.Conn, 0, len(p.clients))
	for c := range p.clients {
		clients = append(clients, c)
	}
	p.mu.Unlock()
	p.listener.Close()
	p.server.Close()
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
func configureDirect(raw string) string {
	target, err := directOrigin(raw)
	if err != nil {
		return failure(err)
	}
	directState.Lock()
	defer directState.Unlock()
	if old := directState.current; old != nil {
		if old.target.String() == target.String() {
			return encoded(map[string]any{"ok": true, "proxy": "http://" + old.listener.Addr().String()})
		}
		old.close()
		directState.current = nil
	}
	listener, err := net.Listen("tcp4", "127.0.0.1:0")
	if err != nil {
		return failure(err)
	}
	p := &directProxy{target: target, listener: listener, clients: make(map[net.Conn]bool)}
	p.reverse = httputil.NewSingleHostReverseProxy(target)
	p.reverse.Transport = &http.Transport{Proxy: nil, DialContext: (&net.Dialer{Timeout: 5 * time.Second}).DialContext, ResponseHeaderTimeout: 15 * time.Second, ForceAttemptHTTP2: true}
	p.server = &http.Server{Handler: p, ReadHeaderTimeout: 5 * time.Second, MaxHeaderBytes: 65536}
	directState.current = p
	go p.server.Serve(trackedListener{Listener: listener, owner: p})
	return encoded(map[string]any{"ok": true, "proxy": "http://" + listener.Addr().String()})
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
		upstream, err := (&net.Dialer{}).DialContext(ctx, "tcp", authority(p.target))
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
	p.reverse.ServeHTTP(w, r)
}
