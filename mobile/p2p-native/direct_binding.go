package main

import (
	"bytes"
	"context"
	"crypto/tls"
	"crypto/x509"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/netip"
	"net/url"
	"slices"
	"time"
)

// Stored by the existing Android profile owner after an explicit successful
// save-and-connect. Recovery may validate this binding, never replace it.
type directBinding struct {
	SchemaVersion int          `json:"schemaVersion"`
	Origin        string       `json:"origin"`
	Addresses     []netip.Addr `json:"addresses"`
}

type directNetwork struct {
	lookup func(context.Context, string, string) ([]netip.Addr, error)
	dial   func(context.Context, string, string) (net.Conn, error)
	roots  *x509.CertPool
}

func systemDirectNetwork() directNetwork {
	// Preserve platform DNS routing, including Android's cgo/private-DNS path.
	// StrictErrors helps the Go backend, but does not enforce cgo completeness.
	resolver := &net.Resolver{PreferGo: net.DefaultResolver.PreferGo, StrictErrors: true, Dial: net.DefaultResolver.Dial}
	return directNetwork{lookup: func(ctx context.Context, network, host string) ([]netip.Addr, error) {
		if network != "ip" {
			return nil, errors.New("需要完整的地址族解析")
		}
		return lookupDirectFamilies(ctx, resolver.LookupNetIP, verifyDirectAbsence, host)
	}, dial: (&net.Dialer{Timeout: 5 * time.Second}).DialContext}
}

// A combined lookup may return A while AAAA failed, on both Go and cgo.
// In cgo these select AF_INET/AF_INET6, never AF_UNSPEC aggregation. Android
// additionally requires status-preserving evidence before accepting absence.
func lookupDirectFamilies(ctx context.Context, lookup func(context.Context, string, string) ([]netip.Addr, error), absence func(context.Context, string, string) error, host string) ([]netip.Addr, error) {
	var addresses []netip.Addr
	for _, family := range []string{"ip4", "ip6"} {
		answers, err := lookup(ctx, family, host)
		if err != nil {
			var dns *net.DNSError
			if errors.As(err, &dns) && dns.IsNotFound && !dns.IsTemporary && !dns.IsTimeout && ctx.Err() == nil {
				if err := absence(ctx, family, host); err != nil {
					return nil, err
				}
				continue
			}
			return nil, err
		}
		if len(answers) == 0 {
			if err := absence(ctx, family, host); err != nil {
				return nil, err
			}
		}
		addresses = append(addresses, answers...)
	}
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	return addresses, nil
}

func allowedDirectAddress(address netip.Addr) bool {
	if !address.IsValid() || address.Zone() != "" {
		return false
	}
	address = address.Unmap()
	if !address.IsGlobalUnicast() || address.IsLoopback() || address.IsLinkLocalUnicast() {
		return false
	}
	if address.Is4() {
		v4 := address.As4()
		return v4[0] != 0 && v4[0] < 224
	}
	return !netip.MustParsePrefix("fec0::/10").Contains(address)
}

func directAnswers(ctx context.Context, target *url.URL, network directNetwork) ([]netip.Addr, error) {
	if address, err := netip.ParseAddr(target.Hostname()); err == nil {
		if !allowedDirectAddress(address) {
			return nil, errors.New("不能连接保留地址")
		}
		return []netip.Addr{address.Unmap()}, nil
	}
	answers, err := network.lookup(ctx, "ip", target.Hostname())
	if err != nil {
		return nil, fmt.Errorf("无法解析服务器地址: %w", err)
	}
	if len(answers) == 0 || len(answers) > 64 {
		return nil, errors.New("服务器地址解析集合无效")
	}
	addresses := make([]netip.Addr, len(answers))
	for i, answer := range answers {
		if !allowedDirectAddress(answer) {
			return nil, errors.New("服务器解析包含保留地址，请重新配置")
		}
		addresses[i] = answer.Unmap()
	}
	slices.SortFunc(addresses, func(a, b netip.Addr) int { return a.Compare(b) })
	return slices.Compact(addresses), nil
}

func parseDirectBinding(target *url.URL, raw string) (directBinding, error) {
	// Literal IPs are already an explicit numeric destination; they never use DNS.
	if address, err := netip.ParseAddr(target.Hostname()); err == nil && allowedDirectAddress(address) {
		return directBinding{1, target.String(), []netip.Addr{address.Unmap()}}, nil
	}
	var binding directBinding
	decoder := json.NewDecoder(bytes.NewBufferString(raw))
	decoder.DisallowUnknownFields()
	if len(raw) > 8192 || decoder.Decode(&binding) != nil || decoder.Decode(new(any)) != io.EOF ||
		binding.SchemaVersion != 1 || binding.Origin != target.String() || len(binding.Addresses) == 0 || len(binding.Addresses) > 64 {
		return binding, errors.New("请返回连接页保存并连接，确认服务器地址")
	}
	for i, address := range binding.Addresses {
		if !allowedDirectAddress(address) || address != address.Unmap() || (i > 0 && !binding.Addresses[i-1].Less(address)) {
			return binding, errors.New("保存的服务器地址绑定无效，请重新确认")
		}
	}
	return binding, nil
}

func validateDirectBinding(ctx context.Context, target *url.URL, binding directBinding, network directNetwork) (string, error) {
	answers, err := directAnswers(ctx, target, network)
	if err != nil {
		return "", err
	}
	if !slices.Equal(answers, binding.Addresses) {
		return "", errors.New("服务器解析地址已改变，请返回连接页保存并连接以重新确认")
	}
	_, port, err := net.SplitHostPort(authority(target))
	if err != nil {
		return "", err
	}
	return net.JoinHostPort(binding.Addresses[0].String(), port), nil
}

// Used by the native attempt owner, before any workspace cookie is available.
// The returned binding is persisted only after the Android generation check.
func checkDirectTarget(ctx context.Context, raw, saved string, confirm bool, network directNetwork) (directBinding, error) {
	target, err := directOrigin(raw)
	if err != nil {
		return directBinding{}, err
	}
	var binding directBinding
	var address string
	if confirm {
		answers, resolveErr := directAnswers(ctx, target, network)
		if resolveErr != nil {
			return binding, resolveErr
		}
		binding = directBinding{1, target.String(), answers}
		_, port, _ := net.SplitHostPort(authority(target))
		address = net.JoinHostPort(answers[0].String(), port)
	} else {
		binding, err = parseDirectBinding(target, saved)
		if err == nil {
			address, err = validateDirectBinding(ctx, target, binding, network)
		}
		if err != nil {
			return binding, err
		}
	}
	transport := &http.Transport{Proxy: nil, TLSClientConfig: &tls.Config{ServerName: target.Hostname(), RootCAs: network.roots, MinVersion: tls.VersionTLS12},
		DialContext: func(ctx context.Context, _, requested string) (net.Conn, error) {
			if requested != authority(target) {
				return nil, errors.New("Unconfigured target")
			}
			return network.dial(ctx, "tcp", address)
		}}
	defer transport.CloseIdleConnections()
	client := &http.Client{Transport: transport, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
	request, err := http.NewRequestWithContext(ctx, http.MethodGet, target.String()+"/api/version", nil)
	if err != nil {
		return binding, err
	}
	response, err := client.Do(request)
	if err != nil {
		return binding, err
	}
	defer response.Body.Close()
	var version struct {
		WebCompatVersion int    `json:"webCompatVersion"`
		APIVersion       string `json:"apiVersion"`
		KernelVersion    string `json:"kernelVersion"`
	}
	if response.StatusCode != http.StatusOK || json.NewDecoder(io.LimitReader(response.Body, 65536)).Decode(&version) != nil ||
		version.WebCompatVersion <= 0 || version.APIVersion == "" || version.KernelVersion == "" {
		return binding, errors.New("这个地址不是 Neige 服务器")
	}
	if err := ctx.Err(); err != nil {
		return binding, err
	}
	return binding, nil
}

func checkDirectWithPermit(raw, saved string, confirm bool, permit *operationPermit) string {
	ctx, cancel := context.WithTimeout(permit.ctx, 5*time.Second)
	defer cancel()
	binding, err := checkDirectTarget(ctx, raw, saved, confirm, systemDirectNetwork())
	if err != nil {
		return failure(err)
	}
	return encoded(map[string]any{"ok": true, "binding": binding})
}
