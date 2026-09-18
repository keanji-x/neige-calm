package main

import (
	"encoding/json"
	"fmt"
	"net/netip"
	"strings"
	"testing"

	"tailscale.com/ipn/ipnstate"
	"tailscale.com/tailcfg"
	"tailscale.com/types/key"
	"tailscale.com/types/views"
)

func targetStatus(t *testing.T, peers ...*ipnstate.PeerStatus) *ipnstate.Status {
	t.Helper()
	s := &ipnstate.Status{BackendState: "Running", Peer: make(map[key.NodePublic]*ipnstate.PeerStatus)}
	for i, peer := range peers {
		var public key.NodePublic
		if err := public.UnmarshalText([]byte(fmt.Sprintf("nodekey:%064x", i+1))); err != nil {
			t.Fatal(err)
		}
		s.Peer[public] = peer
	}
	return s
}

func targetPeer() *ipnstate.PeerStatus {
	return &ipnstate.PeerStatus{ID: "stable-server-a", DNSName: "alpha.tail.example.", InNetworkMap: true,
		TailscaleIPs: []netip.Addr{netip.MustParseAddr("100.100.1.2"), netip.MustParseAddr("fd7a:115c:a1e0::2")}}
}

func TestTailnetTargetSelectsTwoPeersAndAddressFamilies(t *testing.T) {
	alpha := targetPeer()
	beta := &ipnstate.PeerStatus{ID: "stable-server-b", DNSName: "beta.other.example.", InNetworkMap: true,
		TailscaleIPs: []netip.Addr{netip.MustParseAddr("fd7a:115c:a1e0::9")}}
	s := targetStatus(t, alpha, beta)
	for _, tc := range []struct {
		origin, authority, address, host string
		id                               tailcfg.StableNodeID
	}{
		{"https://alpha.tail.example:10000", "alpha.tail.example:10000", "100.100.1.2:10000", "alpha.tail.example", alpha.ID},
		{"https://beta.other.example", "beta.other.example:443", "[fd7a:115c:a1e0::9]:443", "beta.other.example", beta.ID},
	} {
		binding, err := resolveTailnetTarget(tc.origin, s)
		if err != nil {
			t.Fatal(err)
		}
		if binding.SchemaVersion != 1 || binding.Origin != tc.origin || binding.PeerID != tc.id {
			t.Fatalf("unexpected binding: %+v", binding)
		}
		destination, err := validateTailnetTarget(binding, s)
		if err != nil {
			t.Fatal(err)
		}
		if destination.Origin != tc.origin || destination.Hostname != tc.host || destination.Authority != tc.authority || destination.Address.String() != tc.address {
			t.Fatalf("origin, SNI, CONNECT or dial address diverged: %+v", destination)
		}
	}
}

func TestTailnetTargetNormalizesPeerDNSAndSelectsDeterministically(t *testing.T) {
	peer := targetPeer()
	peer.DNSName = "AlPhA.TaIl.ExAmPlE."
	peer.TailscaleIPs = []netip.Addr{netip.MustParseAddr("fd7a:115c:a1e0::2"), netip.MustParseAddr("100.100.1.9"), netip.MustParseAddr("100.100.1.2")}
	binding, err := resolveTailnetTarget("https://alpha.tail.example:1", targetStatus(t, peer))
	if err != nil || binding.TailnetIP.String() != "100.100.1.2" {
		t.Fatalf("deterministic IPv4 selection: %+v, %v", binding, err)
	}
	peer.DNSName = "alpha.tail.example"
	peer.TailscaleIPs = []netip.Addr{netip.MustParseAddr("fd7a:115c:a1e0::9"), netip.MustParseAddr("fd7a:115c:a1e0::2")}
	binding, err = resolveTailnetTarget("https://alpha.tail.example:65535", targetStatus(t, peer))
	if err != nil || binding.TailnetIP.String() != "fd7a:115c:a1e0::2" {
		t.Fatalf("deterministic IPv6 selection: %+v, %v", binding, err)
	}
}

func TestTailnetTargetRejectsInvalidOrigins(t *testing.T) {
	for _, origin := range []string{
		"", "alpha.tail.example", "http://alpha.tail.example", "HTTPS://alpha.tail.example", "https:alpha.tail.example",
		"https://alpha.tail.example/", "https://alpha.tail.example/next", "https://alpha.tail.example?", "https://alpha.tail.example?q=1",
		"https://alpha.tail.example#", "https://alpha.tail.example#v1.secret", "https://user@alpha.tail.example", "https://user:secret@alpha.tail.example",
		"https://alpha.tail.example:", "https://alpha.tail.example:0", "https://alpha.tail.example:65536", "https://alpha.tail.example:-1",
		"https://alpha.tail.example:0443", "https://alpha.tail.example:443", "https://alpha.tail.example:1.0", "https://alpha.tail.example:80:90",
		"https://ALPHA.tail.example", "https://alpha.tail.example.", "https://alpha..tail.example", "https://-alpha.tail.example", "https://alpha-.tail.example",
		"https://alpha_tail.example", "https://alpha%2etail.example", "https://alpha.tail.example\\evil", "https://é.tail.example",
		"https://localhost", "https://foo.localhost", "https://singlelabel", "https://100.100.1.2", "https://127.1", "https://2130706433",
		"https://0x7f000001", "https://0x7f.0x1", "https://[fd7a:115c:a1e0::2]", "https://[fe80::1%25eth0]", " https://alpha.tail.example",
		"https://" + strings.Repeat("a", 64) + ".example", "https://" + strings.Repeat("a.", 127) + "a",
	} {
		if _, err := parseTailnetOrigin(origin); err == nil {
			t.Errorf("accepted invalid origin %q", origin)
		}
	}
}

func TestTailnetTargetRejectsUnavailablePeerState(t *testing.T) {
	for _, state := range []string{"", "Starting", "NeedsLogin", "NeedsMachineAuth", "Stopped"} {
		s := targetStatus(t, targetPeer())
		s.BackendState = state
		if _, err := resolveTailnetTarget("https://alpha.tail.example", s); err == nil {
			t.Errorf("accepted backend state %q", state)
		}
	}
	for _, s := range []*ipnstate.Status{nil, targetStatus(t), targetStatus(t, nil)} {
		if _, err := resolveTailnetTarget("https://alpha.tail.example", s); err == nil {
			t.Error("accepted missing peer state")
		}
	}
	for _, change := range []func(*ipnstate.PeerStatus){
		func(p *ipnstate.PeerStatus) { p.InNetworkMap = false },
		func(p *ipnstate.PeerStatus) { p.Expired = true },
		func(p *ipnstate.PeerStatus) { p.ID = "" },
		func(p *ipnstate.PeerStatus) { p.DNSName = "other.tail.example." },
		func(p *ipnstate.PeerStatus) { p.DNSName = "alpha.tail.example.." },
		func(p *ipnstate.PeerStatus) { p.DNSName = ""; p.HostName = "alpha.tail.example" },
		func(p *ipnstate.PeerStatus) { p.TailscaleIPs = nil; p.Addrs = []string{"100.100.1.2:10000"} },
		func(p *ipnstate.PeerStatus) {
			p.TailscaleIPs = nil
			routes := views.SliceOf([]netip.Prefix{netip.MustParsePrefix("100.100.1.2/32")})
			p.AllowedIPs = &routes
			p.PrimaryRoutes = &routes
		},
	} {
		peer := targetPeer()
		change(peer)
		if _, err := resolveTailnetTarget("https://alpha.tail.example", targetStatus(t, peer)); err == nil {
			t.Errorf("accepted unavailable peer: %+v", peer)
		}
	}
}

func TestTailnetTargetRejectsAmbiguousPeerBindings(t *testing.T) {
	for _, conflict := range []string{"dns", "id", "ip"} {
		alpha := targetPeer()
		beta := &ipnstate.PeerStatus{ID: "stable-server-b", DNSName: "beta.tail.example.", InNetworkMap: true,
			TailscaleIPs: []netip.Addr{netip.MustParseAddr("100.100.1.9")}}
		switch conflict {
		case "dns":
			beta.DNSName = "ALPHA.tail.example."
		case "id":
			beta.ID = alpha.ID
		case "ip":
			beta.TailscaleIPs = alpha.TailscaleIPs
		}
		if _, err := resolveTailnetTarget("https://alpha.tail.example", targetStatus(t, alpha, beta)); err == nil {
			t.Errorf("accepted ambiguous %s", conflict)
		}
		beta.InNetworkMap = false
		if _, err := resolveTailnetTarget("https://alpha.tail.example", targetStatus(t, alpha, beta)); err != nil {
			t.Errorf("stale, non-network-map %s peer blocked current binding: %v", conflict, err)
		}
	}
}

func TestTailnetTargetRejectsNonNodeAddresses(t *testing.T) {
	for _, raw := range []string{"", "0.0.0.0", "127.0.0.1", "169.254.1.2", "224.0.0.1", "192.168.1.2", "8.8.8.8",
		"100.63.255.255", "100.128.0.0", "::", "::1", "fe80::1", "ff02::1", "2001:db8::1", "fd00::1", "fd7a:115c:a1e1::1",
		"::ffff:100.100.1.2", "fd7a:115c:a1e0::2%eth0"} {
		ip, _ := netip.ParseAddr(raw)
		peer := targetPeer()
		peer.TailscaleIPs = []netip.Addr{ip}
		if _, err := resolveTailnetTarget("https://alpha.tail.example", targetStatus(t, peer)); err == nil {
			t.Errorf("accepted non-node address %q", raw)
		}
		peer.TailscaleIPs = append(peer.TailscaleIPs, netip.MustParseAddr("100.100.1.2"))
		if _, err := resolveTailnetTarget("https://alpha.tail.example", targetStatus(t, peer)); err == nil {
			t.Errorf("accepted mixed node/non-node addresses %q", raw)
		}
	}
	for _, raw := range []string{"100.64.0.0", "100.127.255.255", "fd7a:115c:a1e0::", "fd7a:115c:a1e0:ffff:ffff:ffff:ffff:ffff"} {
		peer := targetPeer()
		peer.TailscaleIPs = []netip.Addr{netip.MustParseAddr(raw)}
		if _, err := resolveTailnetTarget("https://alpha.tail.example", targetStatus(t, peer)); err != nil {
			t.Errorf("rejected in-prefix node address %s: %v", raw, err)
		}
	}
}

func TestTailnetTargetRestoreRejectsPeerIDReassignment(t *testing.T) {
	peer := targetPeer()
	s := targetStatus(t, peer)
	binding, err := resolveTailnetTarget("https://alpha.tail.example:10000", s)
	if err != nil {
		t.Fatal(err)
	}
	peer.ID = "replacement-same-name-and-ip"
	if _, err := validateTailnetTarget(binding, s); err == nil {
		t.Fatal("silently accepted another stable peer at the original DNS name and IP")
	}
}

func TestTailnetTargetRestoreRejectsDNSAndIPChanges(t *testing.T) {
	for _, change := range []func(*ipnstate.PeerStatus){
		func(p *ipnstate.PeerStatus) { p.DNSName = "renamed.tail.example." },
		func(p *ipnstate.PeerStatus) {
			p.TailscaleIPs = []netip.Addr{netip.MustParseAddr("100.100.1.9"), netip.MustParseAddr("fd7a:115c:a1e0::2")}
		},
	} {
		peer := targetPeer()
		s := targetStatus(t, peer)
		binding, err := resolveTailnetTarget("https://alpha.tail.example", s)
		if err != nil {
			t.Fatal(err)
		}
		change(peer)
		if _, err := validateTailnetTarget(binding, s); err == nil {
			t.Fatalf("silently replaced the saved binding after peer change: %+v", peer)
		}
	}
}

func TestTailnetTargetRestorePreservesConfirmedBinding(t *testing.T) {
	peer := targetPeer()
	peer.TailscaleIPs = []netip.Addr{netip.MustParseAddr("fd7a:115c:a1e0::2")}
	binding, err := resolveTailnetTarget("https://alpha.tail.example:10000", targetStatus(t, peer))
	if err != nil {
		t.Fatal(err)
	}
	raw, err := json.Marshal(binding)
	if err != nil {
		t.Fatal(err)
	}
	var saved tailnetBinding
	if err := json.Unmarshal(raw, &saved); err != nil {
		t.Fatal(err)
	}
	peer.TailscaleIPs = append(peer.TailscaleIPs, netip.MustParseAddr("100.100.1.2"))
	peer.CurAddr = "203.0.113.1:12345"
	peer.Addrs = []string{"192.168.1.2:12345"}
	peer.Relay = "changed-derp"
	peer.DNSName = "ALPHA.tail.example"
	peer.Online = false
	peer.Active = false
	s := targetStatus(t, nil, peer) // Rotate the status map's current node key.
	destination, err := validateTailnetTarget(saved, s)
	if err != nil {
		t.Fatal(err)
	}
	if saved != binding || destination.Address.String() != "[fd7a:115c:a1e0::2]:10000" {
		t.Fatalf("restore changed saved IPv6 binding: %+v, %+v", saved, destination)
	}
}

func TestTailnetTargetRestoreRejectsIncompleteBindings(t *testing.T) {
	peer := targetPeer()
	s := targetStatus(t, peer)
	for _, raw := range []string{
		`{}`, `{"schemaVersion":2,"origin":"https://alpha.tail.example","peerId":"stable-server-a","tailnetIp":"100.100.1.2"}`,
		`{"origin":"https://alpha.tail.example","peerId":"stable-server-a","tailnetIp":"100.100.1.2"}`,
		`{"schemaVersion":1,"peerId":"stable-server-a","tailnetIp":"100.100.1.2"}`,
		`{"schemaVersion":1,"origin":"https://alpha.tail.example","tailnetIp":"100.100.1.2"}`,
		`{"schemaVersion":1,"origin":"https://alpha.tail.example","peerId":"stable-server-a"}`,
		`{"schemaVersion":1,"origin":"https://ALPHA.tail.example","peerId":"stable-server-a","tailnetIp":"100.100.1.2"}`,
		`{"schemaVersion":1,"origin":"https://alpha.tail.example","peerId":"stable-server-a","tailnetIp":"127.0.0.1"}`,
	} {
		var binding tailnetBinding
		if err := json.Unmarshal([]byte(raw), &binding); err != nil {
			t.Fatal(err)
		}
		if _, err := validateTailnetTarget(binding, s); err == nil {
			t.Errorf("accepted incomplete or invalid binding %s", raw)
		}
	}
}
