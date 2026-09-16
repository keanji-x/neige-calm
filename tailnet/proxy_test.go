package main

import (
	"context"
	"encoding/json"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"
	"time"
)

func TestFixedUpstreamRejectsNetworkTargetsAndNoncanonicalSockets(t *testing.T) {
	for _, raw := range []string{"http://127.0.0.1:4051", "https://example.com", "ingress.sock", "/private/../ingress.sock", "/private/app.sock", "/" + strings.Repeat("x", 104) + "/ingress.sock"} {
		if _, err := fixedUpstream(raw); err == nil {
			t.Fatalf("accepted %s", raw)
		}
	}
}
func TestProxyLocksUpstreamAndStripsUntrustedIdentity(t *testing.T) {
	socket := unixUpstream(t, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		for _, key := range []string{"Forwarded", "X-Forwarded-Host", "X-Forwarded-For", "Tailscale-User-Login", "X-Tailscale-User", "X-Real-IP"} {
			if r.Header.Get(key) != "" {
				t.Errorf("forwarded %s", key)
			}
		}
		if r.Header.Get("X-Calm-Actor") != "user" {
			t.Error("untrusted actor forwarded")
		}
		if r.URL.Path != "/api/version" || r.Host == "evil.invalid" {
			t.Error("request target escaped fixed ingress")
		}
		io.WriteString(w, "fixed-ingress")
	}))
	target, err := fixedUpstream(socket)
	if err != nil {
		t.Fatal(err)
	}
	var trapRequests atomic.Int32
	trap := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { trapRequests.Add(1); io.WriteString(w, "wrong-ingress") }))
	defer trap.Close()
	req := httptest.NewRequest("GET", trap.URL+"/api/version", nil)
	req.Host = "evil.invalid"
	for _, key := range []string{"Forwarded", "X-Forwarded-Host", "X-Forwarded-For", "Tailscale-User-Login", "X-Tailscale-User", "X-Real-IP", "X-Calm-Actor"} {
		req.Header.Set(key, "secret-identity")
	}
	result := httptest.NewRecorder()
	restrictedProxy(upstreamTransport(target)).ServeHTTP(result, req)
	if trapRequests.Load() != 0 {
		t.Error("request authority overrode fixed upstream")
	}
	if result.Code != 200 || result.Body.String() != "fixed-ingress" {
		t.Fatalf("unexpected response %d %s", result.Code, result.Body.String())
	}
}
func TestProxyReportsKernelDowntimeWithoutRedirect(t *testing.T) {
	target := filepath.Join(t.TempDir(), "ingress.sock")
	result := httptest.NewRecorder()
	restrictedProxy(upstreamTransport(target)).ServeHTTP(result, httptest.NewRequest("GET", "/api/version", nil))
	if result.Code != 503 || !strings.Contains(result.Body.String(), "tailnet_upstream_unavailable") {
		t.Fatal(result)
	}
}
func TestRevocationClosesUpgradedTransport(t *testing.T) {
	raw, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	listener := newTrackedListener(raw)
	server := &http.Server{Handler: http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		c, b, err := w.(http.Hijacker).Hijack()
		if err != nil {
			t.Error(err)
			return
		}
		defer c.Close()
		b.WriteString("HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n")
		b.Flush()
		io.Copy(c, c)
	})}
	go server.Serve(listener)
	defer server.Close()
	conn, err := net.Dial("tcp", raw.Addr().String())
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	conn.SetDeadline(time.Now().Add(2 * time.Second))
	io.WriteString(conn, "GET / HTTP/1.1\r\nHost: fixture\r\n\r\n")
	buffer := make([]byte, 1024)
	if _, err = conn.Read(buffer); err != nil {
		t.Fatal(err)
	}
	listener.closeAll()
	if _, err = conn.Read(buffer); err == nil {
		t.Fatal("upgraded transport survived revocation")
	}
	if timeout, ok := err.(net.Error); ok && timeout.Timeout() {
		t.Fatal("upgraded transport stayed open until timeout")
	}

}
func TestStateLockAndVersionPreventTwoWritersAndUnsafeRollback(t *testing.T) {
	dir := t.TempDir()
	os.Chmod(dir, 0700)
	lock, err := lockState(dir)
	if err != nil {
		t.Fatal(err)
	}
	defer lock.Close()
	if another, err := lockState(dir); err == nil {
		another.Close()
		t.Fatal("two writers acquired node state")
	}
	if err = checkStateVersion(dir); err != nil {
		t.Fatal(err)
	}
	os.WriteFile(filepath.Join(dir, "state-version"), []byte("unknown-new-version"), 0600)
	if err = checkStateVersion(dir); err == nil {
		t.Fatal("unsafe state rollback accepted")
	}
}
func TestControlStatusNeverReturnsLoginURL(t *testing.T) {
	target := filepath.Join(t.TempDir(), "ingress.sock")
	service := newService(nil, target)
	host, peer := net.Pipe()
	defer peer.Close()
	go handleControl(context.Background(), host, service)
	io.WriteString(peer, "{\"version\":1,\"action\":\"status\"}\n")
	var response response
	if err := json.NewDecoder(peer).Decode(&response); err != nil {
		t.Fatal(err)
	}
	if response.LoginURL != nil || response.Version != 1 {
		t.Fatal("login data escaped status")
	}
}

func unixUpstream(t *testing.T, handler http.Handler) string {
	t.Helper()
	socket := filepath.Join(t.TempDir(), "ingress.sock")
	listener, err := net.Listen("unix", socket)
	if err != nil {
		t.Fatal(err)
	}
	server := &http.Server{Handler: handler}
	go server.Serve(listener)
	t.Cleanup(func() { server.Close() })
	return socket
}
