package main

import (
	"context"
	"errors"
	"io"
	"log"
	"net"
	"net/http"
	"net/http/httputil"
	"net/url"
	"path/filepath"
	"strings"
	"time"
)

func fixedUpstream(raw string) (string, error) {
	if !filepath.IsAbs(raw) || filepath.Clean(raw) != raw || filepath.Base(raw) != "ingress.sock" || len(raw) >= 104 {
		return "", errors.New("upstream must be the fixed private ingress Unix socket")
	}
	return raw, nil
}

func upstreamTransport(socket string) *http.Transport {
	dialer := &net.Dialer{Timeout: 3 * time.Second, KeepAlive: 30 * time.Second}
	return &http.Transport{
		Proxy: nil,
		// Neither URL authority nor HTTP/forwarded headers can select a target.
		DialContext: func(ctx context.Context, _, _ string) (net.Conn, error) {
			return dialer.DialContext(ctx, "unix", socket)
		},
		ResponseHeaderTimeout: 30 * time.Second, IdleConnTimeout: 60 * time.Second,
	}
}
func restrictedProxy(transport *http.Transport) http.Handler {
	target := &url.URL{Scheme: "http", Host: "neige-private-ingress"}
	proxy := &httputil.ReverseProxy{
		Rewrite: func(r *httputil.ProxyRequest) {
			r.SetURL(target)
			r.Out.Host = target.Host
			for key := range r.Out.Header {
				lower := strings.ToLower(key)
				if strings.HasPrefix(lower, "tailscale-") || strings.HasPrefix(lower, "x-tailscale-") || strings.HasPrefix(lower, "x-forwarded-") || lower == "forwarded" || lower == "x-real-ip" {
					r.Out.Header.Del(key)
				}
			}
			r.Out.Header.Set("X-Calm-Actor", "user")
		},
		Transport: transport, FlushInterval: -1, ErrorLog: log.New(io.Discard, "", 0),
		ErrorHandler: func(w http.ResponseWriter, r *http.Request, err error) {
			w.Header().Set("Content-Type", "application/json")
			w.Header().Set("Cache-Control", "no-store")
			w.WriteHeader(http.StatusServiceUnavailable)
			io.WriteString(w, `{"error":{"code":"tailnet_upstream_unavailable","message":"Neige is restarting. Retry shortly."}}`)
		},
	}
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method == http.MethodConnect {
			http.Error(w, "CONNECT is unavailable", http.StatusMethodNotAllowed)
			return
		}
		proxy.ServeHTTP(w, r)
	})
}
func probeUpstream(ctx context.Context, transport *http.Transport) bool {
	ctx, cancel := context.WithTimeout(ctx, 2*time.Second)
	defer cancel()
	req, _ := http.NewRequestWithContext(ctx, http.MethodGet, "http://neige-private-ingress/api/version", nil)
	res, err := transport.RoundTrip(req)
	if err != nil {
		return false
	}
	defer res.Body.Close()
	return res.StatusCode == http.StatusOK
}
