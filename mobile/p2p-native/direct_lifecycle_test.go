package main

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"net/netip"
	"net/url"
	"sync/atomic"
	"testing"
	"time"
)

func TestDirectColdInstallDoesNotResolveAndMissingBindingCannotGrantAuthority(t *testing.T) {
	original := net.DefaultResolver
	var resolutions atomic.Int32
	net.DefaultResolver = &net.Resolver{PreferGo: true, Dial: func(context.Context, string, string) (net.Conn, error) {
		resolutions.Add(1)
		return nil, errors.New("offline DNS fixture")
	}}
	t.Cleanup(func() { stopDirect(); net.DefaultResolver = original })
	var result struct {
		OK    bool   `json:"ok"`
		Proxy string `json:"proxy"`
	}
	if err := json.Unmarshal([]byte(configureDirect("https://cold.example", "")), &result); err != nil || !result.OK {
		t.Fatalf("local install failed: %v", err)
	}
	if resolutions.Load() != 0 {
		t.Fatal("cold local install waited for DNS")
	}
	if status := absoluteProxyGet(t, result.Proxy, "https://cold.example"); status == 200 {
		t.Fatal("missing confirmation granted network authority")
	}
	if resolutions.Load() != 0 {
		t.Fatal("missing binding attempted fresh DNS adoption")
	}
}

func TestDirectMalformedBindingsAndIncompleteAnswersFailClosed(t *testing.T) {
	f := directDNS(t)
	for _, binding := range []string{"", `{}`, `{"schemaVersion":1,"origin":"https://other.example","addresses":["192.168.1.8"]}`,
		`{"schemaVersion":1,"origin":"` + f.origin + `","addresses":["127.0.0.1"]}`,
		`{"schemaVersion":1,"origin":"` + f.origin + `","addresses":["192.168.1.8","192.168.1.8"]}`} {
		if _, err := checkDirectTarget(context.Background(), f.origin, binding, false, f.network); err == nil {
			t.Fatalf("accepted invalid binding: %s", binding)
		}
	}
	f.set()
	if _, err := checkDirectTarget(context.Background(), f.origin, "", true, f.network); err == nil {
		t.Fatal("accepted empty DNS answer set")
	}
	if len(f.dials) != 0 {
		t.Fatal("invalid binding or incomplete DNS reached network")
	}
}

func TestDirectLiteralLANUsesNoDNSAndCannotFollowRedirects(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		http.Redirect(w, r, "http://127.0.0.1/secret", http.StatusFound)
	}))
	defer server.Close()
	var dials int
	network := directNetwork{lookup: func(context.Context, string, string) ([]netip.Addr, error) {
		t.Fatal("literal IP performed DNS")
		return nil, nil
	},
		dial: func(ctx context.Context, network, address string) (net.Conn, error) {
			dials++
			if address != "192.168.1.8:4140" {
				t.Fatalf("unexpected redirect/dial: %s", address)
			}
			return (&net.Dialer{}).DialContext(ctx, network, server.Listener.Addr().String())
		}}
	if _, err := checkDirectTarget(context.Background(), "http://192.168.1.8:4140", "", false, network); err == nil {
		t.Fatal("redirect treated as Neige proof")
	}
	if dials != 1 {
		t.Fatalf("redirect dialed: %d", dials)
	}
}

func TestDirectRetiredProxyRejectsLateNumericDial(t *testing.T) {
	target, _ := directOrigin("http://192.168.1.8:4140")
	entered, release := make(chan struct{}), make(chan struct{})
	connection, peer := net.Pipe()
	defer peer.Close()
	network := systemDirectNetwork()
	network.dial = func(context.Context, string, string) (net.Conn, error) {
		close(entered)
		<-release
		return connection, nil
	}
	p := newDirectProxy(target, "", network)
	ctx, err := p.authorize(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	done := make(chan error, 1)
	go func() { _, err := p.dial(ctx, "tcp", authority(target)); done <- err }()
	<-entered
	p.close()
	close(release)
	if err := <-done; err == nil {
		t.Fatal("retired proxy accepted a late dial")
	}
	peer.SetReadDeadline(time.Now().Add(time.Second))
	if _, err := peer.Read(make([]byte, 1)); err != io.EOF {
		t.Fatalf("late connection not closed: %v", err)
	}
}

func TestDirectOriginReplacementCancelsPendingDNSAndRetiresOldPort(t *testing.T) {
	origin := "https://first.example"
	binding := encoded(directBinding{1, origin, []netip.Addr{netip.MustParseAddr("192.168.1.8")}})
	var configured struct {
		OK    bool   `json:"ok"`
		Proxy string `json:"proxy"`
	}
	json.Unmarshal([]byte(configureDirect(origin, binding)), &configured)
	if !configured.OK {
		t.Fatal("configure")
	}
	defer stopDirect()
	old := directState.current
	entered, returned := make(chan struct{}), make(chan struct{})
	old.network.lookup = func(ctx context.Context, _, _ string) ([]netip.Addr, error) {
		close(entered)
		<-ctx.Done()
		close(returned)
		return nil, ctx.Err()
	}
	done := make(chan struct{})
	go func() {
		defer close(done)
		endpoint, _ := url.Parse(configured.Proxy)
		client := &http.Client{Transport: &http.Transport{Proxy: http.ProxyURL(endpoint)}, Timeout: 2 * time.Second}
		response, err := client.Get(origin + "/api/version")
		if err == nil {
			response.Body.Close()
		}
	}()
	<-entered
	if json.Unmarshal([]byte(configureDirect("http://192.168.1.9:4140", "")), &configured) != nil || !configured.OK {
		t.Fatal("replacement")
	}
	select {
	case <-returned:
	case <-time.After(time.Second):
		t.Fatal("old DNS was not cancelled")
	}
	<-done
	if old.ctx.Err() == nil {
		t.Fatal("old origin retained authority")
	}
	if connection, err := net.DialTimeout("tcp", old.listener.Addr().String(), time.Second); err == nil {
		connection.Close()
		t.Fatal("old port is still accepting")
	}
}

func TestDirectConfirmationCancellationCannotProduceBinding(t *testing.T) {
	f := directDNS(t)
	entered := make(chan struct{})
	f.network.lookup = func(ctx context.Context, _, _ string) ([]netip.Addr, error) {
		close(entered)
		<-ctx.Done()
		return nil, ctx.Err()
	}
	admission := &operationAdmission{}
	token, err := admission.reserve()
	if err != nil {
		t.Fatal(err)
	}
	permit, err := admission.claim(token)
	if err != nil {
		t.Fatal(err)
	}
	done := make(chan error, 1)
	go func() { _, err := checkDirectTarget(permit.ctx, f.origin, "", true, f.network); done <- err }()
	<-entered
	if err := admission.revoke(token); err != nil {
		t.Fatal(err)
	}
	select {
	case err := <-done:
		if err == nil {
			t.Fatal("cancelled proof produced binding")
		}
	case <-time.After(time.Second):
		t.Fatal("confirmation ignored cancellation")
	}
	if len(f.dials) != 0 {
		t.Fatal("cancelled proof dialed")
	}
}
