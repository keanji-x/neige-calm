package main

/*
#include <stdlib.h>
*/
import "C"

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/netip"
	"os"
	"path/filepath"
	"sync"
	"time"

	"tailscale.com/tailcfg"
	"tailscale.com/tsnet"
)

const targetHost = "pivot-neige.tail328551.ts.net:10000"
const targetIP = "100.123.126.35"
const targetAddress = targetIP + ":10000"
const targetURL = "https://" + targetHost

var instance struct {
	sync.Mutex
	engine *engine
}

type engine struct {
	node    *tsnet.Server
	proxy   net.Listener
	started time.Time
	client  *http.Client
}

func encoded(value any) string { b, _ := json.Marshal(value); return string(b) }
func failure(err error) string { return encoded(map[string]any{"ok": false, "error": err.Error()}) }
func current() (*engine, error) {
	instance.Lock()
	defer instance.Unlock()
	if instance.engine == nil {
		return nil, fmt.Errorf("连接尚未启动")
	}
	return instance.engine, nil
}

func start(dir string) string {
	instance.Lock()
	defer instance.Unlock()
	if instance.engine != nil {
		return encoded(map[string]any{"ok": true})
	}
	if !filepath.IsAbs(dir) {
		return failure(fmt.Errorf("需要应用私有目录"))
	}
	if err := os.MkdirAll(dir, 0700); err != nil {
		return failure(err)
	}
	namePath := filepath.Join(dir, "trial-hostname")
	name, err := os.ReadFile(namePath)
	if os.IsNotExist(err) {
		var suffix [4]byte
		if _, err = rand.Read(suffix[:]); err != nil {
			return failure(err)
		}
		name = []byte("neige-p2p-" + hex.EncodeToString(suffix[:]))
		err = os.WriteFile(namePath, name, 0600)
	}
	if err != nil {
		return failure(err)
	}
	node := &tsnet.Server{Dir: dir, Hostname: string(name), Logf: func(string, ...any) {}, UserLogf: func(string, ...any) {}}
	if err = node.Start(); err != nil {
		return failure(err)
	}
	listener, err := net.Listen("tcp4", "127.0.0.1:0")
	if err != nil {
		node.Close()
		return failure(err)
	}
	e := &engine{node: node, proxy: listener, started: time.Now()}
	e.client = &http.Client{Timeout: 20 * time.Second, Transport: &http.Transport{DialContext: e.dial, ForceAttemptHTTP2: true}, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
	instance.engine = e
	server := &http.Server{Handler: e, ReadHeaderTimeout: 5 * time.Second, MaxHeaderBytes: 8192}
	go server.Serve(listener)
	return encoded(map[string]any{"ok": true})
}

func (e *engine) dial(ctx context.Context, network, address string) (net.Conn, error) {
	if network != "tcp" || address != targetHost {
		return nil, fmt.Errorf("只允许连接已指定的工作区")
	}
	return e.node.Dial(ctx, "tcp", targetAddress)
}

func status() string {
	e, err := current()
	if err != nil {
		return failure(err)
	}
	lc, err := e.node.LocalClient()
	if err != nil {
		return failure(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 4*time.Second)
	defer cancel()
	s, err := lc.Status(ctx)
	if err != nil {
		return failure(err)
	}
	result := map[string]any{"ok": true, "state": s.BackendState, "authURL": s.AuthURL, "origin": targetURL, "elapsedMs": time.Since(e.started).Milliseconds(), "path": "unknown", "health": s.Health}
	return encoded(result)
}

// An explicit user action can retry enrollment when automatic startup has not
// produced an authorization URL. Never log or persist the one-time URL.
func login() string {
	e, err := current()
	if err != nil {
		return failure(err)
	}
	lc, err := e.node.LocalClient()
	if err != nil {
		return failure(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	state, err := lc.Status(ctx)
	if err != nil {
		return failure(err)
	}
	if state.BackendState == "Running" {
		return status()
	}
	if state.AuthURL == "" {
		if err := lc.StartLoginInteractive(ctx); err != nil {
			return failure(err)
		}
	}
	for {
		state, err = lc.Status(ctx)
		if err != nil {
			return failure(fmt.Errorf("获取登录链接失败，请检查当前网络后重试：%w", err))
		}
		if state.AuthURL != "" || state.BackendState == "Running" {
			return status()
		}
		select {
		case <-ctx.Done():
			return failure(fmt.Errorf("暂时无法获取登录链接（%s）。请检查现有 VPN 是否允许本 App 联网，再点登录重试。", state.BackendState))
		case <-time.After(300 * time.Millisecond):
		}
	}
}

func probe() string {
	e, err := current()
	if err != nil {
		return failure(err)
	}
	begin := time.Now()
	response, err := e.client.Get(targetURL + "/api/version")
	if err != nil {
		return failure(err)
	}
	defer response.Body.Close()
	body, err := io.ReadAll(io.LimitReader(response.Body, 65537))
	if err != nil {
		return failure(err)
	}
	if len(body) > 65536 {
		return failure(fmt.Errorf("接口返回超过试验上限"))
	}
	var version map[string]any
	if response.StatusCode != 200 {
		return failure(fmt.Errorf("服务器返回 HTTP %d", response.StatusCode))
	}
	if err = json.Unmarshal(body, &version); err != nil {
		return failure(err)
	}
	result := map[string]any{"ok": true, "requestMs": time.Since(begin).Milliseconds(), "bytes": len(body), "webCompatVersion": version["webCompatVersion"], "path": "unknown"}
	lc, err := e.node.LocalClient()
	if err != nil {
		return failure(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	ping, err := lc.Ping(ctx, netip.MustParseAddr(targetIP), tailcfg.PingDisco)
	if err == nil && ping.Err == "" {
		result["latencyMs"] = ping.LatencySeconds * 1000
		if ping.Endpoint != "" {
			result["path"] = "direct"
		} else if ping.DERPRegionID != 0 {
			result["path"] = "relay"
			result["relay"] = ping.DERPRegionCode
		}
	}
	return encoded(result)
}

func (e *engine) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodConnect || r.Host != targetHost || r.URL.Host != targetHost {
		http.Error(w, "Target denied", http.StatusForbidden)
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), 15*time.Second)
	defer cancel()
	upstream, err := e.dial(ctx, "tcp", r.Host)
	if err != nil {
		http.Error(w, "Workspace connection unavailable", http.StatusBadGateway)
		return
	}
	defer upstream.Close()
	local, buffer, err := w.(http.Hijacker).Hijack()
	if err != nil {
		return
	}
	defer local.Close()
	if _, err = buffer.WriteString("HTTP/1.1 200 Connection Established\r\n\r\n"); err != nil {
		return
	}
	if err = buffer.Flush(); err != nil {
		return
	}
	local.SetDeadline(time.Now().Add(30 * time.Minute))
	upstream.SetDeadline(time.Now().Add(30 * time.Minute))
	done := make(chan struct{})
	go func() { io.Copy(upstream, buffer); upstream.Close(); local.Close(); close(done) }()
	io.Copy(local, upstream)
	local.Close()
	upstream.Close()
	<-done
}

//export p2pStart
func p2pStart(dir *C.char) *C.char { return C.CString(start(C.GoString(dir))) }

//export p2pStatus
func p2pStatus() *C.char { return C.CString(status()) }

//export p2pLogin
func p2pLogin() *C.char { return C.CString(login()) }

//export p2pProbe
func p2pProbe() *C.char { return C.CString(probe()) }

//export p2pProxy
func p2pProxy() *C.char {
	e, err := current()
	if err != nil {
		return C.CString("")
	}
	return C.CString("http://" + e.proxy.Addr().String())
}
func main() {}
