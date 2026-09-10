package main

import (
	"net/http"
	"net/http/httptest"
	"testing"
)

func TestProxyRejectsOtherTargetsBeforeDial(t *testing.T) {
	e := &engine{}
	for _, tc := range []struct{ method, url string }{{"CONNECT", "http://evil.example:443"}, {"GET", targetURL}, {"CONNECT", "http://127.0.0.1:4140"}, {"CONNECT", "http://pivot-neige.tail328551.ts.net:443"}} {
		req := httptest.NewRequest(tc.method, tc.url, nil)
		response := httptest.NewRecorder()
		e.ServeHTTP(response, req)
		if response.Code != http.StatusForbidden {
			t.Fatalf("%s %s: got %d", tc.method, tc.url, response.Code)
		}
	}
}
