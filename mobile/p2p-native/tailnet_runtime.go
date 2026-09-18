package main

import (
	"context"
	"crypto/tls"
	"errors"
	"net"
	"sync"
	"time"

	"tailscale.com/client/local"
	"tailscale.com/ipn"
	"tailscale.com/ipn/ipnstate"
	"tailscale.com/logtail"
	"tailscale.com/tsnet"
)

// Quiet callbacks do not stop tsnet's independent disk/upload logger. Disable
// it before constructing our only app-private node; system tailscaled is a
// different process and is unaffected.
func privateTailnetNode(dir, hostname string) *tsnet.Server {
	logtail.Disable()
	return &tsnet.Server{Dir: dir, Hostname: hostname, Logf: func(string, ...any) {}, UserLogf: func(string, ...any) {}}
}

// The engine owns one node. All enrollment, validation and application dials
// use this port; tests replace the node, not the enrollment orchestration.
type tailnetRuntime interface {
	Status(context.Context) (*ipnstate.Status, error)
	StartAuth(context.Context, string) error
	HasIdentity(context.Context) (bool, error)
	Logout(context.Context) error
	Dial(context.Context, string, string) (net.Conn, error)
}

type realTailnetRuntime struct {
	node      *tsnet.Server
	local     *local.Client
	mutations sync.Mutex
}

func (r *realTailnetRuntime) Status(ctx context.Context) (*ipnstate.Status, error) {
	return r.local.Status(ctx)
}
func (r *realTailnetRuntime) StartAuth(ctx context.Context, key string) error {
	r.mutations.Lock()
	defer r.mutations.Unlock()
	if err := ctx.Err(); err != nil {
		return err
	}
	prefs, err := r.local.GetPrefs(ctx)
	if err != nil || prefs == nil {
		return errors.New("无法读取手机入网配置")
	}
	// A fresh/just-logged-out profile has default prefs. Restore the same
	// node's explicit settings before starting, as tsnet.Start does.
	prefs.Hostname = r.node.Hostname
	prefs.ControlURL = r.node.ControlURL
	prefs.WantRunning = true
	prefs.LoggedOut = false
	if err := r.local.Start(ctx, ipn.Options{AuthKey: key, UpdatePrefs: prefs}); err != nil {
		return err
	}
	status, err := r.local.Status(ctx)
	if err != nil {
		return err
	}
	if !status.HaveNodeKey {
		// This is also the pinned CLI's --auth-key path: the SDK name refers
		// to starting registration, not opening a browser. No URL is consumed
		// by this app, and the key remains in the native control client.
		return r.local.StartLoginInteractive(ctx)
	}
	return nil
}

// Read only registration markers through our app-local API. A machine key or
// an empty state file is created before enrollment and is not a network identity.
func (r *realTailnetRuntime) HasIdentity(ctx context.Context) (bool, error) {
	prefs, err := r.local.GetPrefs(ctx)
	if err != nil {
		return false, errors.New("无法确认已保存的节点身份")
	}
	if prefs == nil {
		return false, errors.New("无法确认已保存的节点身份")
	}
	if p := prefs.Persist; p != nil {
		if !p.NodeID.IsZero() || p.UserProfile.ID != 0 || p.UserProfile.LoginName != "" || !p.OldPrivateNodeKey.IsZero() {
			return true, nil
		}
	}
	current, profiles, err := r.local.ProfileStatus(ctx)
	if err != nil {
		return false, errors.New("无法确认已保存的节点身份")
	}
	return current.ID != "" || len(profiles) != 0, nil
}
func (r *realTailnetRuntime) Logout(ctx context.Context) error {
	r.mutations.Lock()
	defer r.mutations.Unlock()
	if err := ctx.Err(); err != nil {
		return err
	}
	if err := r.local.Logout(ctx); err != nil {
		return err
	}
	prefs, err := r.local.GetPrefs(ctx)
	if err != nil || prefs == nil {
		return errors.New("无法读取手机入网配置")
	}
	prefs.WantRunning = false
	prefs.LoggedOut = true
	// SDK Logout returns early when there is no persisted node key, even if
	// its control client is still registering. Start replaces that client with
	// a stopped, keyless-auth client, preserving the SDK's current Persist.
	// The second Logout expires any identity committed between the first
	// early return and that replacement. Registered logout still uses the
	// original authenticated control client before dropping its auth key.
	if err := r.local.Start(ctx, ipn.Options{UpdatePrefs: prefs}); err != nil {
		return err
	}
	return r.local.Logout(ctx)
}
func (r *realTailnetRuntime) Dial(ctx context.Context, network, address string) (net.Conn, error) {
	return r.node.Dial(ctx, network, address)
}

func (e *engine) runtime() (tailnetRuntime, error) {
	e.mu.Lock()
	defer e.mu.Unlock()
	if e.transport == nil {
		return nil, errors.New("连接正在恢复")
	}
	return e.transport, nil
}

// TLS is still end-to-end. This independent handshake checks the candidate
// before saving it; WebView repeats normal certificate/SNI verification.
func (e *engine) verifyTargetTLS(ctx context.Context, binding tailnetBinding) error {
	runtime, err := e.runtime()
	if err != nil {
		return err
	}
	status, err := runtime.Status(ctx)
	if err != nil {
		return errors.New("无法读取可信节点状态")
	}
	destination, err := validateTailnetTarget(binding, status)
	if err != nil {
		return err
	}
	connection, err := runtime.Dial(ctx, "tcp", destination.Address.String())
	if err != nil {
		return errors.New("无法连接目标节点")
	}
	defer connection.Close()
	secure := tls.Client(connection, &tls.Config{ServerName: destination.Hostname, MinVersion: tls.VersionTLS12, RootCAs: e.tlsRoots})
	if err := secure.HandshakeContext(ctx); err != nil {
		return errors.New("目标节点 TLS 验证失败")
	}
	return nil
}

// A changed network map invalidates live tunnels as well as future dials.
// A failed watcher also closes the tunnel: no stale trust while reconnecting.
func (e *engine) watchTargets(client *local.Client) {
	for {
		watcher, err := client.WatchIPNBus(context.Background(), ipn.NotifyInitialStatus|ipn.NotifyPeerChanges)
		if err == nil {
			for {
				if _, err = watcher.Next(); err != nil {
					break
				}
				ctx, cancel := context.WithTimeout(context.Background(), 4*time.Second)
				status, statusErr := client.Status(ctx)
				cancel()
				e.enrollmentMu.Lock()
				if tunnel := e.tunnel; tunnel != nil {
					_, validationErr := validateTailnetTarget(tunnel.binding, status)
					if statusErr != nil || validationErr != nil {
						tunnel.invalidateNetwork()
					}
				}
				e.enrollmentMu.Unlock()
			}
			watcher.Close()
		}
		e.enrollmentMu.Lock()
		if e.tunnel != nil {
			e.tunnel.invalidateNetwork()
		}
		e.enrollmentMu.Unlock()
		time.Sleep(time.Second)
	}
}
