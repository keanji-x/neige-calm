package main

import (
	"io"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"
)

type directTestTransport struct {
	calls   int
	headers http.Header
}

func (t *directTestTransport) RoundTrip(r *http.Request) (*http.Response, error) {
	t.calls++
	t.headers = r.Header.Clone()
	return &http.Response{StatusCode: 200, Header: make(http.Header), Body: io.NopCloser(strings.NewReader("upstream"))}, nil
}
func TestDirectProxyRejectsUnconfiguredRedirectTargets(t *testing.T) {
	target, _ := url.Parse("http://192.168.1.8:4140")
	transport := &directTestTransport{}
	reverse := directForwarder(target)
	reverse.Transport = transport
	proxy := &directProxy{target: target, reverse: reverse}
	for _, raw := range []string{"http://192.168.1.9:4140/api/version", "http://192.168.1.8:4141/api/version", "http://169.254.169.254/api/version", "http://user@192.168.1.8:4140/api/version"} {
		response := httptest.NewRecorder()
		proxy.ServeHTTP(response, httptest.NewRequest(http.MethodGet, raw, nil))
		if response.Code != 403 {
			t.Fatalf("unconfigured hop %s got %d", raw, response.Code)
		}
	}
	if transport.calls != 0 {
		t.Fatal("Unconfigured request reached upstream")
	}
	request := httptest.NewRequest(http.MethodConnect, "http://192.168.1.9:443", nil)
	response := httptest.NewRecorder()
	proxy.ServeHTTP(response, request)
	if response.Code != 403 {
		t.Fatal("foreign CONNECT accepted")
	}
}
func TestDirectOriginRejectsImplicitLocalAndCredentialTargets(t *testing.T) {
	for _, raw := range []string{"http://localhost:4140", "http://127.0.0.1:4140", "http://169.254.169.254", "http://example.com", "https://user:password@example.com", "http://224.0.0.1"} {
		if _, err := directOrigin(raw); err == nil {
			t.Fatalf("accepted %s", raw)
		}
	}
	for _, raw := range []string{"http://192.168.1.8:4140", "http://203.0.113.8:4140", "https://calm.example.com"} {
		if _, err := directOrigin(raw); err != nil {
			t.Fatalf("rejected %s: %v", raw, err)
		}
	}
}

func TestDirectProxyDoesNotForgeForwardedIdentity(t *testing.T) {
	target, _ := url.Parse("http://192.168.1.8:4140")
	transport := &directTestTransport{}
	reverse := directForwarder(target)
	reverse.Transport = transport
	proxy := &directProxy{target: target, reverse: reverse}
	request := httptest.NewRequest(http.MethodGet, target.String()+"/api/version", nil)
	for _, header := range []string{"Forwarded", "X-Forwarded-For", "X-Forwarded-Host", "X-Forwarded-Proto", "X-Real-IP"} {
		request.Header.Set(header, "127.0.0.1")
	}
	response := httptest.NewRecorder()
	proxy.ServeHTTP(response, request)
	if response.Code != 200 || transport.calls != 1 {
		t.Fatal("Valid request must be forwarded")
	}
	for _, header := range []string{"Forwarded", "X-Forwarded-For", "X-Forwarded-Host", "X-Forwarded-Proto", "X-Real-IP"} {
		if transport.headers.Get(header) != "" {
			t.Fatalf("Forwarded identity header %s", header)
		}
	}
}
