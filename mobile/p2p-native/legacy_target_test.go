package main

import (
	"crypto/x509"
	"net/netip"
	"os"
	"path/filepath"
	"testing"
)

func legacyFixture(t *testing.T) (*engine, *enrollmentNode) {
	t.Helper()
	e, node, _ := enrollmentFixture(t, true)
	for _, peer := range node.state.Peer {
		peer.DNSName = "pivot-neige.tail328551.ts.net."
		peer.TailscaleIPs = []netip.Addr{netip.MustParseAddr("100.123.126.35")}
	}
	node.registered = true
	return e, node
}

func TestLegacyV1ConfirmationCanBindRetainedIdentityWithoutV2Enrollment(t *testing.T) {
	e, node := legacyFixture(t)
	// Previous v1 releases have a working SDK identity but no target file.
	// This is the native bind called by the launcher's explicit v1 confirm.
	if err := e.confirmLegacyTarget("https://pivot-neige.tail328551.ts.net:10000", testOperationPermit(t)); err != nil {
		t.Fatal(err)
	}
	if _, err := e.bindTailnet("https://pivot-neige.tail328551.ts.net:10000"); err != nil {
		t.Fatalf("retained v1 confirmation cannot recover its existing node: %v", err)
	}
	if node.authCalls != 0 || node.logoutCalls != 0 {
		t.Fatal("legacy confirmation changed identity")
	}
	if len(node.addresses) != 1 || node.addresses[0] != "100.123.126.35:10000" {
		t.Fatal("legacy confirmation bypassed numeric TLS target")
	}
}

func TestLegacyConfirmationNeverRepairsAnExistingSavedPeerMismatch(t *testing.T) {
	e, node := legacyFixture(t)
	if err := e.confirmLegacyTarget(legacyTailnetOrigin, testOperationPermit(t)); err != nil {
		t.Fatal(err)
	}
	before, err := os.ReadFile(filepath.Join(e.dir, "tailnet-targets.json"))
	if err != nil {
		t.Fatal(err)
	}
	for _, peer := range node.state.Peer {
		peer.ID = "replacement"
	}
	if err := e.confirmLegacyTarget(legacyTailnetOrigin, testOperationPermit(t)); err == nil {
		t.Fatal("silently rebound saved peer")
	}
	after, err := os.ReadFile(filepath.Join(e.dir, "tailnet-targets.json"))
	if err != nil || string(before) != string(after) {
		t.Fatal("modified saved peer fence")
	}
}

func TestLegacyConfirmationRejectsForeignOriginWrongAddressTLSAndRetiredPermit(t *testing.T) {
	for _, kind := range []string{"foreign", "address", "tls", "cancelled", "not-running", "malformed-record"} {
		t.Run(kind, func(t *testing.T) {
			e, node := legacyFixture(t)
			origin := legacyTailnetOrigin
			permit := testOperationPermit(t)
			switch kind {
			case "foreign":
				origin = "https://other.tail.example"
			case "address":
				for _, peer := range node.state.Peer {
					peer.TailscaleIPs = []netip.Addr{netip.MustParseAddr("100.123.126.36")}
				}
			case "tls":
				e.tlsRoots = x509.NewCertPool()
			case "cancelled":
				if err := permit.revoke(); err != nil {
					t.Fatal(err)
				}
			case "not-running":
				node.state.BackendState = "NeedsLogin"
			case "malformed-record":
				if err := os.WriteFile(filepath.Join(e.dir, "tailnet-targets.json"), []byte("{broken"), 0600); err != nil {
					t.Fatal(err)
				}
			}
			if err := e.confirmLegacyTarget(origin, permit); err == nil {
				t.Fatal("unsafe legacy migration accepted")
			}
			if node.authCalls != 0 || node.logoutCalls != 0 {
				t.Fatal("legacy confirmation mutated identity")
			}
			if _, err := savedTarget(e.dir, legacyTailnetOrigin); err == nil {
				t.Fatal("failed migration committed a target")
			}
		})
	}
}
