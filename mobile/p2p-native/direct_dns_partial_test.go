package main

import (
	"context"
	"crypto/tls"
	"net"
	"net/http"
	"net/netip"
	"net/url"
	"sync"
	"testing"
	"time"

	"golang.org/x/net/dns/dnsmessage"
)

// Only the DNS authority and final TLS destination are fixtures. Resolution is
// the actual production factory, followed by the real binding/proxy/probe code.
func partialDNSAuthority(t *testing.T, failure dnsmessage.RCode) (func(dnsmessage.RCode), func() map[dnsmessage.Type]int) {
	t.Helper()
	socket, err := net.ListenPacket("udp4", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	var mu sync.Mutex
	code := failure
	queries := map[dnsmessage.Type]int{}
	done := make(chan struct{})
	go func() {
		defer close(done)
		for {
			buffer := make([]byte, 4096)
			n, remote, err := socket.ReadFrom(buffer)
			if err != nil {
				return
			}
			var query dnsmessage.Message
			if query.Unpack(buffer[:n]) != nil {
				continue
			}
			reply := dnsmessage.Message{Header: dnsmessage.Header{ID: query.ID, Response: true, RecursionAvailable: true}, Questions: query.Questions}
			for _, question := range query.Questions {
				mu.Lock()
				queries[question.Type]++
				current := code
				mu.Unlock()
				if question.Type == dnsmessage.TypeA {
					reply.Answers = append(reply.Answers, dnsmessage.Resource{Header: dnsmessage.ResourceHeader{Name: question.Name, Type: dnsmessage.TypeA, Class: dnsmessage.ClassINET, TTL: 0}, Body: &dnsmessage.AResource{A: [4]byte{192, 168, 1, 8}}})
				} else if question.Type == dnsmessage.TypeAAAA {
					reply.RCode = current
				}
			}
			response, err := reply.Pack()
			if err == nil {
				_, _ = socket.WriteTo(response, remote)
			}
		}
	}()
	original := net.DefaultResolver
	net.DefaultResolver = &net.Resolver{PreferGo: true, Dial: func(ctx context.Context, _, _ string) (net.Conn, error) {
		return (&net.Dialer{}).DialContext(ctx, "udp4", socket.LocalAddr().String())
	}}
	t.Cleanup(func() { socket.Close(); <-done; net.DefaultResolver = original })
	return func(value dnsmessage.RCode) { mu.Lock(); code = value; mu.Unlock() }, func() map[dnsmessage.Type]int {
		mu.Lock()
		defer mu.Unlock()
		return map[dnsmessage.Type]int{dnsmessage.TypeA: queries[dnsmessage.TypeA], dnsmessage.TypeAAAA: queries[dnsmessage.TypeAAAA]}
	}
}

func TestDirectPartialFamilyDNSCannotConfirm(t *testing.T) {
	_, queries := partialDNSAuthority(t, dnsmessage.RCodeServerFailure)
	f := directDNS(t)
	f.network.lookup = systemDirectNetwork().lookup
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	binding, err := checkDirectTarget(ctx, f.origin, "", true, f.network)
	if queries()[dnsmessage.TypeA] == 0 || queries()[dnsmessage.TypeAAAA] == 0 {
		t.Fatal("fixture did not exercise both DNS families")
	}
	if err == nil || len(f.dials) != 0 {
		t.Fatalf("incomplete A/AAAA set confirmed or dialed: binding=%v dials=%v err=%v", binding, f.dials, err)
	}
}

func TestDirectCompleteIPv4OnlyDNSStillConfirms(t *testing.T) {
	_, queries := partialDNSAuthority(t, dnsmessage.RCodeSuccess)
	f := directDNS(t)
	f.network.lookup = systemDirectNetwork().lookup
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	binding, err := checkDirectTarget(ctx, f.origin, "", true, f.network)
	if err != nil {
		t.Fatal(err)
	}
	if len(binding.Addresses) != 1 || binding.Addresses[0] != netip.MustParseAddr("192.168.1.8") || len(f.dials) != 1 {
		t.Fatalf("IPv4-only control: %v %v", binding, f.dials)
	}
	if queries()[dnsmessage.TypeA] == 0 || queries()[dnsmessage.TypeAAAA] == 0 {
		t.Fatal("fixture did not exercise successful empty AAAA")
	}
}

func TestDirectPartialFamilyDNSCannotRevalidate(t *testing.T) {
	for _, connect := range []bool{false, true} {
		name := "HTTP"
		if connect {
			name = "CONNECT"
		}
		t.Run(name, func(t *testing.T) {
			set, _ := partialDNSAuthority(t, dnsmessage.RCodeSuccess)
			f := directDNS(t)
			f.network.lookup = systemDirectNetwork().lookup
			binding := f.confirmed(t)
			proxy := f.proxy(t, binding)
			set(dnsmessage.RCodeServerFailure)
			if !connect {
				if status := absoluteProxyGet(t, proxy.URL, f.origin); status == 200 {
					t.Error("partial A/AAAA revalidation forwarded HTTP")
				}
			} else {
				endpoint, _ := url.Parse(proxy.URL)
				transport := &http.Transport{Proxy: http.ProxyURL(endpoint), TLSClientConfig: &tls.Config{RootCAs: f.network.roots, MinVersion: tls.VersionTLS12}}
				defer transport.CloseIdleConnections()
				client := &http.Client{Transport: transport, Timeout: 3 * time.Second}
				response, err := client.Get(f.origin + "/api/version")
				if err == nil {
					response.Body.Close()
					if response.StatusCode == 200 {
						t.Error("partial A/AAAA revalidation opened CONNECT")
					}
				}
			}
			f.mu.Lock()
			defer f.mu.Unlock()
			if len(f.dials) != 0 || len(f.hosts) != 0 {
				t.Errorf("partial DNS reached destination: dials=%v hosts=%v", f.dials, f.hosts)
			}
		})
	}
}
