package main

import (
	"context"
	"net/http"
	"net/http/httptest"
	"testing"
)

func TestProxyRejectsOtherTargetsBeforeDial(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	tunnel := &tailnetTunnel{authority: "alpha.tail.example:10000", ctx: ctx, networkCtx: ctx}
	for _, tc := range []struct{ method, url string }{{"CONNECT", "http://evil.example:443"}, {"GET", "https://alpha.tail.example:10000"}, {"CONNECT", "http://127.0.0.1:4140"}, {"CONNECT", "http://alpha.tail.example:443"}} {
		req := httptest.NewRequest(tc.method, tc.url, nil)
		response := httptest.NewRecorder()
		tunnel.ServeHTTP(response, req)
		if response.Code != http.StatusForbidden {
			t.Fatalf("%s %s: got %d", tc.method, tc.url, response.Code)
		}
	}
}
func TestProxyFailsClosedBeforeTailnetStartupCompletes(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	tunnel := &tailnetTunnel{engine: &engine{}, authority: "alpha.tail.example:10000", ctx: ctx, networkCtx: ctx}
	request := httptest.NewRequest(http.MethodConnect, "http://alpha.tail.example:10000", nil)
	request.Host = tunnel.authority
	request.URL.Host = tunnel.authority
	response := httptest.NewRecorder()
	tunnel.ServeHTTP(response, request)
	if response.Code != http.StatusBadGateway {
		t.Fatalf("got %d before node readiness", response.Code)
	}
}
