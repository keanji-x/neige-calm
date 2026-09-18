package main

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"net"
	"net/http"
	"net/url"
	"testing"
	"time"

	"golang.org/x/net/dns/dnsmessage"
)

// Actual direct proxy entry point; only DNS answers and the destination are
// local fixtures. No Tailnet engine, account, system DNS or device is used.
func TestReviewDirectHTTPSRejectsLoopbackDNS(t *testing.T) {
	dns, err := net.ListenPacket("udp4", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer dns.Close()
	go func() {
		data := make([]byte, 4096)
		for {
			n, from, err := dns.ReadFrom(data)
			if err != nil {
				return
			}
			var query dnsmessage.Message
			if query.Unpack(data[:n]) != nil {
				continue
			}
			reply := dnsmessage.Message{Header: dnsmessage.Header{ID: query.ID, Response: true, RecursionAvailable: true}, Questions: query.Questions}
			for _, q := range query.Questions {
				if q.Type == dnsmessage.TypeA {
					reply.Answers = append(reply.Answers, dnsmessage.Resource{Header: dnsmessage.ResourceHeader{Name: q.Name, Type: dnsmessage.TypeA, Class: dnsmessage.ClassINET, TTL: 1}, Body: &dnsmessage.AResource{A: [4]byte{127, 0, 0, 1}}})
				}
			}
			encoded, _ := reply.Pack()
			_, _ = dns.WriteTo(encoded, from)
		}
	}()
	original := net.DefaultResolver
	net.DefaultResolver = &net.Resolver{PreferGo: true, Dial: func(ctx context.Context, _, _ string) (net.Conn, error) {
		return (&net.Dialer{}).DialContext(ctx, "udp4", dns.LocalAddr().String())
	}}
	defer func() { net.DefaultResolver = original }()
	destination, err := net.Listen("tcp4", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer destination.Close()
	accepted := make(chan struct{})
	go func() {
		peer, err := destination.Accept()
		if err == nil {
			close(accepted)
			defer peer.Close()
			_, _ = peer.Read(make([]byte, 1))
		}
	}()
	authority := fmt.Sprintf("neige.review.test:%d", destination.Addr().(*net.TCPAddr).Port)
	var configured struct {
		OK    bool   `json:"ok"`
		Proxy string `json:"proxy"`
	}
	if err := json.Unmarshal([]byte(configureDirect("https://"+authority, "")), &configured); err != nil || !configured.OK {
		t.Fatalf("configuration failed: %v", err)
	}
	defer stopDirect()
	endpoint, _ := url.Parse(configured.Proxy)
	client, err := net.DialTimeout("tcp4", endpoint.Host, time.Second)
	if err != nil {
		t.Fatal(err)
	}
	defer client.Close()
	_ = client.SetDeadline(time.Now().Add(3 * time.Second))
	_, _ = fmt.Fprintf(client, "CONNECT %s HTTP/1.1\r\nHost: %s\r\n\r\n", authority, authority)
	response, err := http.ReadResponse(bufio.NewReader(client), nil)
	if err != nil {
		t.Fatal(err)
	}
	if response.StatusCode == http.StatusOK {
		select {
		case <-accepted:
			t.Fatal("direct HTTPS hostname reached loopback and CONNECT returned 200; design requires rejection before dial")
		case <-time.After(time.Second):
			t.Fatal("CONNECT returned 200 without observed destination")
		}
	}
}
