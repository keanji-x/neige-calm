//go:build linux && cgo

package main

import (
	"context"
	"crypto/tls"
	"net"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
	"time"
)

func TestDirectResolverCGOCompleteness(t *testing.T) {
	if runtime.GOOS != "linux" {
		t.Skip("host libc ABI fixture is Linux-only; Android runtime remains a separate device gate")
	}
	if mode := os.Getenv("NEIGE_TEST_DIRECT_DNS_MODE"); mode != "" {
		// Force the actual cgo backend, rather than exercising StrictErrors in Go.
		net.DefaultResolver = &net.Resolver{}
		f := directDNS(t)
		f.network.lookup = systemDirectNetwork().lookup
		if strings.HasPrefix(mode, "REVALIDATE_") {
			t.Setenv("NEIGE_TEST_DIRECT_DNS_MODE", "AAAA_NODATA")
			binding := f.confirmed(t)
			proxy := f.proxy(t, binding)
			t.Setenv("NEIGE_TEST_DIRECT_DNS_MODE", "AAAA_SERVFAIL")
			if mode == "REVALIDATE_HTTP" {
				if status := absoluteProxyGet(t, proxy.URL, f.origin); status == 200 {
					t.Fatal("cgo partial family forwarded HTTP")
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
						t.Fatal("cgo partial family opened CONNECT")
					}
				}
			}
			f.mu.Lock()
			defer f.mu.Unlock()
			if len(f.dials) != 0 || len(f.hosts) != 0 {
				t.Fatal("cgo partial family reached destination")
			}
			trace, err := os.ReadFile(os.Getenv("NEIGE_TEST_DIRECT_DNS_TRACE"))
			if err != nil {
				t.Fatal(err)
			}
			if strings.Join(strings.Fields(string(trace)), " ") != "2 10 2 10" {
				t.Fatalf("unexpected cgo family calls: %s", trace)
			}
			return
		}
		ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
		defer cancel()
		binding, err := checkDirectTarget(ctx, f.origin, "", true, f.network)
		trace, readErr := os.ReadFile(os.Getenv("NEIGE_TEST_DIRECT_DNS_TRACE"))
		if readErr != nil {
			t.Fatal(readErr)
		}
		calls := strings.Fields(string(trace))
		if strings.HasSuffix(mode, "_NODATA") {
			if err != nil || len(binding.Addresses) != 1 || len(f.dials) != 1 {
				t.Fatalf("explicit no-record control failed: %v %v %v", binding, f.dials, err)
			}
		} else if err == nil || len(f.dials) != 0 {
			t.Fatalf("libc partial/failed family reached confirmation: %v %v %v", binding, f.dials, err)
		}
		want := "2 10"
		if mode == "A_SERVFAIL" {
			want = "2"
		}
		if strings.Join(calls, " ") != want {
			t.Fatalf("cgo must issue AF_INET then AF_INET6, not aggregate: %v", calls)
		}
		return
	}
	compiler, err := exec.LookPath("cc")
	if err != nil {
		t.Fatal(err)
	}
	root := t.TempDir()
	shim := filepath.Join(root, "libdirect-dns-fixture.so")
	build := exec.Command(compiler, "-shared", "-fPIC", "-o", shim, "testdata/direct_getaddrinfo_fixture.c", "-ldl")
	if output, err := build.CombinedOutput(); err != nil {
		t.Fatalf("fixture compile: %v\n%s", err, output)
	}
	binary, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	for _, mode := range []string{"AAAA_SERVFAIL", "AAAA_REFUSED", "A_SERVFAIL", "AAAA_NODATA", "A_NODATA", "REVALIDATE_HTTP", "REVALIDATE_CONNECT"} {
		t.Run(mode, func(t *testing.T) {
			ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
			defer cancel()
			command := exec.CommandContext(ctx, binary, "-test.run=^TestDirectResolverCGOCompleteness$", "-test.v")
			command.Env = append(os.Environ(), "GODEBUG=netdns=cgo", "LD_PRELOAD="+shim,
				"NEIGE_TEST_DIRECT_DNS_MODE="+mode, "NEIGE_TEST_DIRECT_DNS_TRACE="+filepath.Join(root, mode+".trace"))
			if output, err := command.CombinedOutput(); err != nil {
				t.Fatalf("actual cgo child failed: %v\n%s", err, output)
			}
		})
	}
}
