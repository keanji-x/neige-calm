package main

import (
	"context"
	"encoding/json"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"tailscale.com/ipn/ipnstate"
)

// The Rust manager integration test starts this test executable as its helper.
// Only node/API I/O is synthetic; the issuer, ledger and control server are real.
func TestEnrollmentAppControlFixture(t *testing.T) {
	args := os.Args
	dir := ""
	socket := ""
	for n, arg := range args {
		if n+1 < len(args) {
			switch arg {
			case "--state-dir":
				dir = args[n+1]
			case "--control-socket":
				socket = args[n+1]
			}
		}
	}
	if dir == "" {
		t.Skip("private fixture subprocess only")
	}
	for _, arg := range args {
		if arg == "--cleanup-only" || arg == "--cleanup-status" {
			t.Fatal("fixture must never start for stopped-host reads")
		}
	}
	c := enrollmentConfig{SchemaVersion: 1, ClientID: "fixture", SecretFile: filepath.Join(dir, "synthetic.secret"), PhoneTags: []string{"tag:fixture"}, Tailnet: "fixture.example", Origin: "https://fixture.example.ts.net"}
	data, _ := json.Marshal(c)
	configPath := filepath.Join(dir, "fixture.json")
	if err := os.WriteFile(configPath, data, 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(c.SecretFile, []byte("synthetic-fixture-secret"), 0600); err != nil {
		t.Fatal(err)
	}
	i, err := newIssuer(dir, configPath)
	if err != nil {
		t.Fatal(err)
	}
	defer i.dir.Close()
	i.api.client.Transport = roundTripFunc(func(r *http.Request) (*http.Response, error) {
		if strings.HasSuffix(r.URL.Path, "/oauth/token") {
			return responseJSON(`{"access_token":"fixture","token_type":"Bearer","expires_in":3600}`, 200), nil
		}
		if r.Method == "POST" {
			if err := os.WriteFile(filepath.Join(dir, "cloud-post"), []byte("unexpected"), 0600); err != nil {
				t.Fatal(err)
			}
		}
		return responseJSON(`{}`, 503), nil
	})
	n := &enrollmentNode{fakeNode: fakeNode{state: &ipnstate.Status{BackendState: "Running", CurrentTailnet: &ipnstate.TailnetStatus{Name: c.Tailnet}, Self: &ipnstate.PeerStatus{Online: true, DNSName: "fixture.example.ts.net."}}}}
	s := newService(n, filepath.Join(dir, "ingress.sock"))
	s.issuer = i
	s.current.Origin = &c.Origin
	s.current.HTTPSReady = true
	s.current.UpstreamReady = true
	listener, err := net.Listen("unix", socket)
	if err != nil {
		t.Fatal(err)
	}
	defer listener.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	serveControl(ctx, listener, s)
}
