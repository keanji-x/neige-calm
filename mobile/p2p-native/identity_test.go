package main

import (
	"context"
	"net"
	"net/http"
	"net/http/httptest"
	"sync/atomic"
	"testing"
	"time"

	"tailscale.com/derp/derpserver"
	"tailscale.com/net/netns"
	"tailscale.com/tailcfg"
	"tailscale.com/tstest/integration/testcontrol"
	"tailscale.com/types/key"
	"tailscale.com/types/logger"
)

func TestFreshNodeRestartRetainsUnregisteredEligibility(t *testing.T) {
	// Exercise the pinned SDK's actual state writer and LocalAPI. All control
	// requests terminate on loopback; this creates no account or tailnet node.
	control := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { w.WriteHeader(http.StatusServiceUnavailable) }))
	defer control.Close()
	dir := t.TempDir()
	for boot := 0; boot < 2; boot++ {
		node := privateTailnetNode(dir, "scan-local-test")
		node.ControlURL = control.URL
		if err := node.Start(); err != nil {
			t.Fatal(err)
		}
		client, err := node.LocalClient()
		if err != nil {
			node.Close()
			t.Fatal(err)
		}
		ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
		registered, err := (&realTailnetRuntime{node: node, local: client}).HasIdentity(ctx)
		cancel()
		node.Close()
		if err != nil || registered {
			t.Fatalf("unregistered boot %d became an existing identity: registered=%v error=%v", boot, registered, err)
		}
	}
}

func TestPinnedSDKResetStopsAnUnregisteredPendingLogin(t *testing.T) {
	var reachable atomic.Bool
	control := &testcontrol.Server{RequireAuthKey: "tskey-auth-local-fixture", DERPMap: &tailcfg.DERPMap{}, Logf: logger.Discard}
	control.HTTPTestServer = httptest.NewUnstartedServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if !reachable.Load() {
			w.WriteHeader(http.StatusServiceUnavailable)
			return
		}
		control.ServeHTTP(w, r)
	}))
	control.HTTPTestServer.Start()
	defer control.HTTPTestServer.Close()
	node := privateTailnetNode(t.TempDir(), "scan-pending-sdk-test")
	node.ControlURL = control.HTTPTestServer.URL
	if err := node.Start(); err != nil {
		t.Fatal(err)
	}
	defer node.Close()
	client, err := node.LocalClient()
	if err != nil {
		t.Fatal(err)
	}
	runtime := &realTailnetRuntime{node: node, local: client}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if err := runtime.StartAuth(ctx, "tskey-auth-local-fixture"); err != nil {
		t.Fatal(err)
	}
	if known, err := runtime.HasIdentity(ctx); err != nil || known {
		t.Fatalf("fixture must still be unregistered: %v", err)
	}
	e := &engine{dir: node.Dir, transport: runtime}
	if err := e.resetEnrollment(); err != nil {
		t.Fatal(err)
	}
	reachable.Store(true)
	until := time.Now().Add(2 * time.Second)
	for time.Now().Before(until) {
		if known, err := runtime.HasIdentity(ctx); err != nil || known {
			t.Fatalf("cancelled registration continued after successful reset: %v", err)
		}
		time.Sleep(20 * time.Millisecond)
	}
}

func TestPinnedSDKAuthKeyAndConfirmedLogoutBoundary(t *testing.T) {
	// Real tsnet + LocalAPI + control protocol, isolated from production accounts
	// and DERP. This verifies SDK behavior, not cloud policy or an Android device.
	netns.SetEnabled(false)
	t.Cleanup(func() { netns.SetEnabled(true) })
	derp := derpserver.New(key.NewNode(), logger.Discard)
	defer derp.Close()
	relay := httptest.NewTLSServer(derpserver.Handler(derp))
	defer relay.Close()
	derpMap := &tailcfg.DERPMap{Regions: map[int]*tailcfg.DERPRegion{1: {RegionID: 1, RegionCode: "test", Nodes: []*tailcfg.DERPNode{{
		Name: "local", RegionID: 1, HostName: "127.0.0.1", IPv4: "127.0.0.1", IPv6: "none", STUNPort: -1,
		DERPPort: relay.Listener.Addr().(*net.TCPAddr).Port, InsecureForTests: true,
	}}}}}
	control := &testcontrol.Server{RequireAuthKey: "tskey-auth-local-fixture", DERPMap: derpMap, Logf: logger.Discard}
	control.HTTPTestServer = httptest.NewUnstartedServer(control)
	control.HTTPTestServer.Start()
	defer control.HTTPTestServer.Close()
	node := privateTailnetNode(t.TempDir(), "scan-sdk-test")
	node.ControlURL = control.HTTPTestServer.URL
	if err := node.Start(); err != nil {
		t.Fatal(err)
	}
	defer node.Close()
	client, err := node.LocalClient()
	if err != nil {
		t.Fatal(err)
	}
	runtime := &realTailnetRuntime{node: node, local: client}
	ctx, cancel := context.WithTimeout(context.Background(), 6*time.Second)
	defer cancel()
	if err := runtime.StartAuth(ctx, "tskey-auth-local-fixture"); err != nil {
		t.Fatal(err)
	}
	for {
		status, err := runtime.Status(ctx)
		if err != nil {
			t.Fatal(err)
		}
		if status.BackendState == "Running" {
			break
		}
		select {
		case <-ctx.Done():
			t.Fatalf("auth key did not start registration: %s", status.BackendState)
		case <-time.After(20 * time.Millisecond):
		}
	}
	if known, err := runtime.HasIdentity(ctx); err != nil || !known {
		t.Fatalf("registered identity missing: %v", err)
	}
	e := &engine{dir: node.Dir, transport: runtime}
	if err := e.resetEnrollment(); err != nil {
		t.Fatal(err)
	}
	if known, err := runtime.HasIdentity(ctx); err != nil || known {
		t.Fatalf("logout did not clear registration: %v", err)
	}
}
