//go:build linux && cgo

package main

import (
	"context"
	"crypto/tls"
	"net"
	"net/http"
	"net/netip"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"golang.org/x/net/dns/dnsmessage"
)

func TestDirectAndroidAbsenceABI(t *testing.T) {
	if mode := os.Getenv("NEIGE_TEST_ANDROID_MODE"); mode != "" {
		ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
		defer cancel()
		if mode == "timeout" || mode == "late-start" {
			cancel()
			ctx, cancel = context.WithTimeout(context.Background(), 50*time.Millisecond)
			defer cancel()
		}
		f := directDNS(t)
		resolver := &net.Resolver{}
		f.network.lookup = func(ctx context.Context, _ string, host string) ([]netip.Addr, error) {
			return lookupDirectFamilies(ctx, resolver.LookupNetIP, verifyAndroidDirectAbsence, host)
		}
		started := time.Now()
		var err error
		if mode == "revalidate-http" || mode == "revalidate-connect" {
			target, _ := url.Parse(f.origin)
			proxy := f.proxy(t, directBinding{1, f.origin, []netip.Addr{netip.MustParseAddr("192.168.1.8")}})
			if mode == "revalidate-http" {
				if absoluteProxyGet(t, proxy.URL, target.String()) == 200 {
					t.Fatal("ambiguous absence forwarded HTTP")
				}
			} else {
				endpoint, _ := url.Parse(proxy.URL)
				transport := &http.Transport{Proxy: http.ProxyURL(endpoint), TLSClientConfig: &tls.Config{RootCAs: f.network.roots, MinVersion: tls.VersionTLS12}}
				defer transport.CloseIdleConnections()
				client := &http.Client{Transport: transport, Timeout: time.Second}
				response, e := client.Get(f.origin + "/api/version")
				if e == nil {
					response.Body.Close()
					if response.StatusCode == 200 {
						t.Fatal("ambiguous absence opened CONNECT")
					}
				}
			}
		} else {
			_, err = checkDirectTarget(ctx, f.origin, "", true, f.network)
			if mode == "no-data" {
				if err != nil || len(f.dials) != 1 {
					t.Fatalf("no-data proof failed: %v dials=%v", err, f.dials)
				}
			} else if err == nil {
				t.Fatal("ambiguous or failed Android absence confirmed")
			}
		}
		if mode != "no-data" && len(f.dials) != 0 {
			t.Fatalf("unproven absence dialed: %v", f.dials)
		}
		if mode == "missing-api" && !strings.Contains(err.Error(), "literal IP, Tailnet, or Android 10+") {
			t.Fatalf("missing actionable compatibility error: %v", err)
		}
		if (mode == "timeout" || mode == "late-start") && time.Since(started) > 500*time.Millisecond {
			t.Fatal("cancellation did not bound DNS")
		}
		want := "query result"
		if mode == "missing-api" {
			want = ""
		}
		if mode == "start-error" {
			want = "query"
		}
		if mode == "timeout" || mode == "late-start" {
			want = "query cancel"
		}
		deadline := time.Now().Add(time.Second)
		for {
			trace, _ := os.ReadFile(os.Getenv("NEIGE_TEST_ANDROID_TRACE"))
			if strings.Join(strings.Fields(string(trace)), " ") == want {
				break
			}
			if time.Now().After(deadline) {
				t.Fatalf("descriptor ownership trace=%q, want=%q", trace, want)
			}
			time.Sleep(10 * time.Millisecond)
		}
		return
	}
	root := t.TempDir()
	compile := func(output, source string, extra ...string) {
		t.Helper()
		args := append([]string{"-shared", "-fPIC", "-o", output, source, "-ldl"}, extra...)
		if output, err := exec.Command("cc", args...).CombinedOutput(); err != nil {
			t.Fatalf("ABI fixture compilation: %v\n%s", err, output)
		}
	}
	preload := filepath.Join(root, "libgetaddrinfo.so")
	compile(preload, "testdata/direct_getaddrinfo_fixture.c")
	binary, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	for _, mode := range []string{"no-data", "servfail", "header-servfail", "empty-unproven", "missing-api", "start-error", "result-error", "timeout", "late-start", "revalidate-http", "revalidate-connect"} {
		t.Run(mode, func(t *testing.T) {
			directory := filepath.Join(root, mode)
			if err := os.Mkdir(directory, 0700); err != nil {
				t.Fatal(err)
			}
			extra := []string{}
			if mode == "missing-api" {
				extra = append(extra, "-DOMIT_RESULT")
			}
			compile(filepath.Join(directory, "libandroid.so"), "testdata/direct_android_resolver_fixture.c", extra...)
			message := absencePacket()
			if mode == "header-servfail" || strings.HasPrefix(mode, "revalidate-") {
				message.RCode = dnsmessage.RCodeServerFailure
			}
			if mode == "empty-unproven" {
				message.Authorities = nil
			}
			packet, err := message.Pack()
			if err != nil {
				t.Fatal(err)
			}
			packetPath := filepath.Join(directory, "response.dns")
			if err := os.WriteFile(packetPath, packet, 0600); err != nil {
				t.Fatal(err)
			}
			ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
			defer cancel()
			child := exec.CommandContext(ctx, binary, "-test.run=^TestDirectAndroidAbsenceABI$", "-test.v")
			child.Env = append(os.Environ(), "GODEBUG=netdns=cgo", "LD_PRELOAD="+preload, "LD_LIBRARY_PATH="+directory,
				"NEIGE_TEST_DIRECT_DNS_MODE=AAAA_COLLAPSED", "NEIGE_TEST_DIRECT_DNS_TRACE="+filepath.Join(directory, "families.trace"),
				"NEIGE_TEST_ANDROID_MODE="+mode, "NEIGE_TEST_ANDROID_TRACE="+filepath.Join(directory, "ownership.trace"), "NEIGE_TEST_ANDROID_PACKET="+packetPath)
			if output, err := child.CombinedOutput(); err != nil {
				t.Fatalf("Android ABI child failed: %v\n%s", err, output)
			}
		})
	}
}
