package main

import (
	"fmt"
	"net"
	"net/netip"
	"net/url"
	"slices"
	"strconv"
	"strings"

	"tailscale.com/ipn/ipnstate"
	"tailscale.com/tailcfg"
)

// tailnetBinding is a confirmed target, not proof that its peer is still valid.
// Validate it against fresh node status before dialing; never resolve a saved
// origin anew to silently replace its peer or address. All fields are required.
type tailnetBinding struct {
	SchemaVersion int                  `json:"schemaVersion"`
	Origin        string               `json:"origin"`
	PeerID        tailcfg.StableNodeID `json:"peerId"`
	TailnetIP     netip.Addr           `json:"tailnetIp"`
}

// tailnetDestination keeps TLS/HTTP names separate from the numeric tsnet dial
// address. It is valid only for the status snapshot used to validate it.
type tailnetDestination struct {
	Origin    string
	Hostname  string
	Authority string
	Address   netip.AddrPort
}

type tailnetOrigin struct {
	value    string
	hostname string
	port     uint16
}

var (
	targetIPv4 = netip.MustParsePrefix("100.64.0.0/10")
	targetIPv6 = netip.MustParsePrefix("fd7a:115c:a1e0::/48")
)

func targetNodeAddress(ip netip.Addr) bool {
	return ip.Zone() == "" && (targetIPv4.Contains(ip) || targetIPv6.Contains(ip))
}

func targetDNSName(name string) (string, error) {
	name = strings.ToLower(strings.TrimSuffix(name, "."))
	labels := strings.Split(name, ".")
	if len(name) > 253 || len(labels) < 2 {
		return "", fmt.Errorf("需要完整的节点 DNS 名称")
	}
	for _, label := range labels {
		if len(label) == 0 || len(label) > 63 || label[0] == '-' || label[len(label)-1] == '-' {
			return "", fmt.Errorf("无效的节点 DNS 名称")
		}
		for _, c := range label {
			if !(c >= 'a' && c <= 'z' || c >= '0' && c <= '9' || c == '-') {
				return "", fmt.Errorf("无效的节点 DNS 名称")
			}
		}
	}
	// A numeric final label may be interpreted as an abbreviated IP by WebView.
	last := labels[len(labels)-1]
	if last[0] < 'a' || last[0] > 'z' || last == "localhost" {
		return "", fmt.Errorf("需要完整的节点 DNS 名称")
	}
	return name, nil
}

// Input is URL.origin from the launcher, not a URL or a pairing secret. Reject
// noncanonical aliases so the stored origin, WebView fence and TLS name agree.
func parseTailnetOrigin(raw string) (tailnetOrigin, error) {
	invalid := fmt.Errorf("需要规范的 HTTPS 节点地址")
	if !strings.HasPrefix(raw, "https://") || strings.ContainsAny(raw, "?#") {
		return tailnetOrigin{}, invalid
	}
	u, err := url.Parse(raw)
	if err != nil || u.User != nil || u.Opaque != "" || u.Path != "" || u.RawPath != "" {
		return tailnetOrigin{}, invalid
	}
	hostname, err := targetDNSName(u.Hostname())
	if err != nil {
		return tailnetOrigin{}, invalid
	}
	port := uint64(443)
	if u.Port() != "" {
		port, err = strconv.ParseUint(u.Port(), 10, 16)
		if err != nil || port == 0 {
			return tailnetOrigin{}, invalid
		}
	}
	authority := net.JoinHostPort(hostname, strconv.FormatUint(port, 10))
	canonical := "https://" + hostname
	if port != 443 {
		canonical = "https://" + authority
	}
	if raw != canonical {
		return tailnetOrigin{}, invalid
	}
	return tailnetOrigin{value: canonical, hostname: hostname, port: uint16(port)}, nil
}

// targetPeerForName uses only the authenticated node's current network map.
// HostName, AllowedIPs, endpoints and QR-supplied identity are not peer evidence.
func targetPeerForName(hostname string, status *ipnstate.Status) (*ipnstate.PeerStatus, error) {
	if status == nil || status.BackendState != "Running" {
		return nil, fmt.Errorf("Tailscale 尚未连接，无法确认目标节点")
	}
	var selected *ipnstate.PeerStatus
	for _, peer := range status.Peer {
		if peer == nil || !peer.InNetworkMap {
			continue
		}
		name, err := targetDNSName(peer.DNSName)
		if err != nil || name != hostname {
			continue
		}
		if selected != nil {
			return nil, fmt.Errorf("目标节点名称不唯一，请重新确认")
		}
		selected = peer
	}
	if selected == nil || selected.ID.IsZero() || selected.Expired || len(selected.TailscaleIPs) == 0 {
		return nil, fmt.Errorf("找不到有效的目标节点，请重新确认")
	}
	for _, ip := range selected.TailscaleIPs {
		if !targetNodeAddress(ip) {
			return nil, fmt.Errorf("目标节点包含不允许的地址")
		}
	}
	for _, peer := range status.Peer {
		if peer == nil || peer == selected || !peer.InNetworkMap {
			continue
		}
		if peer.ID == selected.ID {
			return nil, fmt.Errorf("目标节点身份不唯一，请重新确认")
		}
		for _, ip := range peer.TailscaleIPs {
			if slices.Contains(selected.TailscaleIPs, ip) {
				return nil, fmt.Errorf("目标节点地址不唯一，请重新确认")
			}
		}
	}
	return selected, nil
}

// resolveTailnetTarget is for an explicit user confirmation, never recovery.
// Prefer IPv4, then the lowest address, independent of map/address ordering.
func resolveTailnetTarget(origin string, status *ipnstate.Status) (tailnetBinding, error) {
	parsed, err := parseTailnetOrigin(origin)
	if err != nil {
		return tailnetBinding{}, err
	}
	peer, err := targetPeerForName(parsed.hostname, status)
	if err != nil {
		return tailnetBinding{}, err
	}
	chosen := peer.TailscaleIPs[0]
	for _, ip := range peer.TailscaleIPs[1:] {
		if ip.Less(chosen) {
			chosen = ip
		}
	}
	return tailnetBinding{SchemaVersion: 1, Origin: parsed.value, PeerID: peer.ID, TailnetIP: chosen}, nil
}

// validateTailnetTarget preserves the confirmed address even if a preferred
// address has since been added. A missing/changed identity or address is an
// error requiring reconfirmation, not permission to pick another destination.
func validateTailnetTarget(binding tailnetBinding, status *ipnstate.Status) (tailnetDestination, error) {
	if binding.SchemaVersion != 1 || binding.PeerID.IsZero() || !targetNodeAddress(binding.TailnetIP) {
		return tailnetDestination{}, fmt.Errorf("目标配置不完整或版本不受支持")
	}
	parsed, err := parseTailnetOrigin(binding.Origin)
	if err != nil {
		return tailnetDestination{}, err
	}
	peer, err := targetPeerForName(parsed.hostname, status)
	if err != nil {
		return tailnetDestination{}, err
	}
	if peer.ID != binding.PeerID {
		return tailnetDestination{}, fmt.Errorf("目标节点身份已改变，请重新确认")
	}
	if !slices.Contains(peer.TailscaleIPs, binding.TailnetIP) {
		return tailnetDestination{}, fmt.Errorf("目标节点地址已改变，请重新确认")
	}
	return tailnetDestination{Origin: parsed.value, Hostname: parsed.hostname,
		Authority: net.JoinHostPort(parsed.hostname, strconv.Itoa(int(parsed.port))),
		Address:   netip.AddrPortFrom(binding.TailnetIP, parsed.port)}, nil
}
