package main

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"io"
	"net"
	"time"
)

const protocolVersion = 1
const maxMessage = 16384

type request struct {
	Version int    `json:"version"`
	Action  string `json:"action"`
}
type status struct {
	DesiredEnabled bool     `json:"desiredEnabled"`
	Phase          string   `json:"phase"`
	ProcessRunning bool     `json:"processRunning"`
	ChildPID       *uint32  `json:"childPid"`
	NodeState      string   `json:"nodeState"`
	HTTPSReady     bool     `json:"httpsReady"`
	UpstreamReady  bool     `json:"upstreamReady"`
	Origin         *string  `json:"origin"`
	DNSName        *string  `json:"dnsName"`
	NodeID         *string  `json:"nodeId"`
	Addresses      []string `json:"addresses"`
	Detail         string   `json:"detail"`
}
type response struct {
	Version  int     `json:"version"`
	Status   status  `json:"status"`
	LoginURL *string `json:"loginUrl"`
	Error    *string `json:"error"`
}

// A filesystem-protected, bounded one-request protocol. It never forwards
// arbitrary localapi requests and never returns AuthURL from ordinary status.
func serveControl(ctx context.Context, listener net.Listener, service *service) {
	go func() { <-ctx.Done(); listener.Close() }()
	for {
		conn, err := listener.Accept()
		if err != nil {
			return
		}
		go handleControl(ctx, conn, service)
	}
}
func handleControl(ctx context.Context, conn net.Conn, service *service) {
	defer conn.Close()
	conn.SetDeadline(time.Now().Add(12 * time.Second))
	line, err := bufio.NewReader(io.LimitReader(conn, maxMessage+1)).ReadBytes('\n')
	if err != nil || len(line) > maxMessage {
		return
	}
	var req request
	decoder := json.NewDecoder(bytes.NewReader(line))
	decoder.DisallowUnknownFields()
	if decoder.Decode(&req) != nil || req.Version != protocolVersion {
		return
	}
	var extra any
	if decoder.Decode(&extra) != io.EOF {
		return
	}
	ctx, cancel := context.WithTimeout(ctx, 10*time.Second)
	defer cancel()
	res := response{Version: protocolVersion}
	switch req.Action {
	case "status":
	case "login":
		res.LoginURL, err = service.login(ctx)
	case "logout":
		err = service.logout(ctx)
	default:
		return
	}
	res.Status = service.snapshot()
	if err != nil {
		message := "Tailnet operation unavailable; retry after checking node status"
		res.Error = &message
	}
	json.NewEncoder(conn).Encode(res)
}
