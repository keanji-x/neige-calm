package main

import (
	"bufio"
	"context"
	"crypto/tls"
	"crypto/x509"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"net/netip"
	"net/url"
	"slices"
	"sync"
	"testing"
	"time"
)

type directDNSFixture struct {
	mu      sync.Mutex
	answers []netip.Addr
	lookups int
	dials   []string
	hosts   []string
	cookies []string
	onDial  func()
	network directNetwork
	origin  string
}

func directDNS(t *testing.T) *directDNSFixture {
	t.Helper()
	f := &directDNSFixture{answers: []netip.Addr{netip.MustParseAddr("192.168.1.8")}}
	server := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		f.mu.Lock()
		f.hosts = append(f.hosts, r.Host)
		f.cookies = append(f.cookies, r.Header.Get("Cookie"))
		f.mu.Unlock()
		fmt.Fprint(w, `{"webCompatVersion":30,"apiVersion":"10","kernelVersion":"fixture"}`)
	}))
	t.Cleanup(server.Close)
	if err := server.Certificate().VerifyHostname("example.com"); err != nil {
		t.Fatal(err)
	}
	_, port, _ := net.SplitHostPort(server.Listener.Addr().String())
	f.origin = "https://example.com:" + port
	roots := x509.NewCertPool()
	roots.AddCert(server.Certificate())
	f.network = directNetwork{roots: roots, lookup: func(ctx context.Context, family, hostname string) ([]netip.Addr, error) {
		if family != "ip" || hostname != "example.com" {
			t.Errorf("not a complete original-hostname query: %s %s", family, hostname)
		}
		f.mu.Lock()
		defer f.mu.Unlock()
		f.lookups++
		return slices.Clone(f.answers), ctx.Err()
	}, dial: func(ctx context.Context, network, address string) (net.Conn, error) {
		host, _, err := net.SplitHostPort(address)
		if _, parseErr := netip.ParseAddr(host); err != nil || parseErr != nil {
			return nil, fmt.Errorf("nonnumeric dial: %s", address)
		}
		f.mu.Lock()
		f.dials = append(f.dials, address)
		next := f.onDial
		f.onDial = nil
		f.mu.Unlock()
		if next != nil {
			next()
		}
		return (&net.Dialer{}).DialContext(ctx, network, server.Listener.Addr().String())
	}}
	return f
}
func (f *directDNSFixture) set(answers ...string) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.answers = nil
	for _, raw := range answers {
		f.answers = append(f.answers, netip.MustParseAddr(raw))
	}
}
func (f *directDNSFixture) confirmed(t *testing.T) directBinding {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	binding, err := checkDirectTarget(ctx, f.origin, "", true, f.network)
	if err != nil {
		t.Fatal(err)
	}
	f.mu.Lock()
	f.lookups = 0
	f.dials = nil
	f.hosts = nil
	f.cookies = nil
	f.mu.Unlock()
	return binding
}
func (f *directDNSFixture) proxy(t *testing.T, binding directBinding) *httptest.Server {
	t.Helper()
	target, _ := directOrigin(f.origin)
	p := newDirectProxy(target, encoded(binding), f.network)
	server := httptest.NewServer(p)
	t.Cleanup(func() { p.close(); server.Close() })
	return server
}
func absoluteProxyGet(t *testing.T, proxy, origin string) int {
	t.Helper()
	address, _ := url.Parse(proxy)
	connection, err := net.DialTimeout("tcp", address.Host, time.Second)
	if err != nil {
		t.Fatal(err)
	}
	defer connection.Close()
	connection.SetDeadline(time.Now().Add(2 * time.Second))
	request, _ := http.NewRequest(http.MethodGet, origin+"/api/version", nil)
	request.Header.Set("Cookie", "calm-session=owned")
	if err := request.WriteProxy(connection); err != nil {
		t.Fatal(err)
	}
	response, err := http.ReadResponse(bufio.NewReader(connection), request)
	if err != nil {
		t.Fatal(err)
	}
	defer response.Body.Close()
	io.Copy(io.Discard, response.Body)
	return response.StatusCode
}

func TestDirectDNSRejectsCompleteMixedOrReservedAnswersBeforeDial(t *testing.T) {
	for _, bad := range []string{"127.0.0.1", "0.0.0.0", "0.1.2.3", "169.254.169.254", "224.0.0.1", "255.255.255.255", "::1", "::", "fe80::1", "ff02::1", "::ffff:127.0.0.1", "fec0::1"} {
		t.Run(bad, func(t *testing.T) {
			f := directDNS(t)
			f.set("192.168.1.8", bad)
			_, err := checkDirectTarget(context.Background(), f.origin, "", true, f.network)
			if err == nil || len(f.dials) != 0 {
				t.Fatalf("mixed reserved answer reached network: %v %v", err, f.dials)
			}
		})
	}
}

func TestDirectDNSPinsOneValidatedNumericDialAndOriginalTLSHostname(t *testing.T) {
	f := directDNS(t)
	binding := f.confirmed(t)
	proxy := f.proxy(t, binding)
	f.onDial = func() { f.set("127.0.0.1") }
	if status := absoluteProxyGet(t, proxy.URL, f.origin); status != 200 {
		t.Fatalf("numeric pinned request: %d", status)
	}
	f.mu.Lock()
	dialHost := ""
	if len(f.dials) == 1 {
		dialHost, _, _ = net.SplitHostPort(f.dials[0])
	}
	if f.lookups != 1 || len(f.dials) != 1 || dialHost != "192.168.1.8" || len(f.hosts) != 1 || f.hosts[0] != authority(proxy.Config.Handler.(*directProxy).target) {
		t.Errorf("lookup/dial/hostname mismatch: %d %v %v", f.lookups, f.dials, f.hosts)
	}
	f.mu.Unlock()
	if status := absoluteProxyGet(t, proxy.URL, f.origin); status == 200 {
		t.Fatal("new HTTP request reused authorization after reserved DNS change")
	}
	f.mu.Lock()
	defer f.mu.Unlock()
	if len(f.hosts) != 1 || len(f.dials) != 1 {
		t.Fatal("changed DNS sent another cookie or dial")
	}
}

func TestDirectDNSChangedSetNeedsExplicitReconfirmationForHTTPAndCONNECT(t *testing.T) {
	for _, connect := range []bool{false, true} {
		t.Run(fmt.Sprint(connect), func(t *testing.T) {
			f := directDNS(t)
			binding := f.confirmed(t)
			proxy := f.proxy(t, binding)
			get := func(proxyURL string) bool {
				if !connect {
					return absoluteProxyGet(t, proxyURL, f.origin) == 200
				}
				endpoint, _ := url.Parse(proxyURL)
				transport := &http.Transport{Proxy: http.ProxyURL(endpoint), DisableKeepAlives: true, TLSClientConfig: &tls.Config{RootCAs: f.network.roots, MinVersion: tls.VersionTLS12}}
				defer transport.CloseIdleConnections()
				client := &http.Client{Transport: transport, Timeout: 2 * time.Second}
				request, _ := http.NewRequest(http.MethodGet, f.origin+"/api/version", nil)
				request.Header.Set("Cookie", "calm-session=owned")
				response, err := client.Do(request)
				if err != nil {
					return false
				}
				defer response.Body.Close()
				io.Copy(io.Discard, response.Body)
				return response.StatusCode == 200
			}
			if !get(proxy.URL) {
				t.Fatal("confirmed request rejected")
			}
			f.set("192.168.1.9")
			if get(proxy.URL) {
				t.Fatal("changed DNS accepted without confirmation")
			}
			f.mu.Lock()
			count := len(f.hosts)
			f.mu.Unlock()
			if count != 1 {
				t.Fatalf("changed DNS sent cookie to upstream: %d", count)
			}
			if _, err := checkDirectTarget(context.Background(), f.origin, encoded(binding), false, f.network); err == nil {
				t.Fatal("passive probe replaced binding")
			}
			reconfirmed := f.confirmed(t)
			if !get(f.proxy(t, reconfirmed).URL) {
				t.Fatal("explicit reconfirmation did not recover")
			}
		})
	}
}

func TestDirectDNSAnswerOrderingIsNotAnAddressChangeButAddedAnswersAre(t *testing.T) {
	f := directDNS(t)
	f.set("fd00::8", "192.168.1.8")
	binding := f.confirmed(t)
	f.set("192.168.1.8", "fd00::8", "192.168.1.8")
	if _, err := checkDirectTarget(context.Background(), f.origin, encoded(binding), false, f.network); err != nil {
		t.Fatal(err)
	}
	f.set("192.168.1.8", "fd00::8", "192.168.1.9")
	if _, err := checkDirectTarget(context.Background(), f.origin, encoded(binding), false, f.network); err == nil {
		t.Fatal("added address silently accepted")
	}
}

func TestDirectDNSTLSRejectsWrongHostnameEvenWithAllowedNumericBinding(t *testing.T) {
	f := directDNS(t)
	f.origin = "https://wrong.example:" + func() string { u, _ := url.Parse(f.origin); return u.Port() }()
	f.network.lookup = func(context.Context, string, string) ([]netip.Addr, error) {
		return []netip.Addr{netip.MustParseAddr("192.168.1.8")}, nil
	}
	if _, err := checkDirectTarget(context.Background(), f.origin, "", true, f.network); err == nil {
		t.Fatal("TLS hostname was replaced by pinned IP or verification disabled")
	}
	proxy := f.proxy(t, directBinding{1, f.origin, []netip.Addr{netip.MustParseAddr("192.168.1.8")}})
	if status := absoluteProxyGet(t, proxy.URL, f.origin); status == 200 {
		t.Fatal("proxy TLS accepted the wrong hostname")
	}
	f.mu.Lock()
	defer f.mu.Unlock()
	if len(f.cookies) != 0 {
		t.Fatal("TLS mismatch forwarded a cookie")
	}
}
