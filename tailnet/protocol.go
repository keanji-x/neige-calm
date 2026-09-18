package main

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"io"
	"net"
	"sync"
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
	var workers sync.WaitGroup
	defer workers.Wait()
	slots := make(chan struct{}, 8)
	for {
		conn, err := listener.Accept()
		if err != nil {
			return
		}
		select {
		case slots <- struct{}{}:
			workers.Add(1)
			go func() { defer workers.Done(); defer func() { <-slots }(); handleControl(ctx, conn, service) }()
		default:
			conn.Close()
		}
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
	var envelope struct {
		Version int `json:"version"`
	}
	if json.Unmarshal(line, &envelope) == nil && envelope.Version == 2 {
		handleEnrollment(ctx, conn, service, line)
		return
	}
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

type enrollmentRequest struct {
	Version int               `json:"version"`
	Command enrollmentCommand `json:"command"`
}
type enrollmentResponse struct {
	Version int               `json:"version"`
	Result  *enrollmentResult `json:"result"`
	Error   *string           `json:"error"`
}

func handleEnrollment(ctx context.Context, conn net.Conn, s *service, line []byte) {
	var req enrollmentRequest
	var fields map[string]json.RawMessage
	if !exactFields(line, "version", "command") || strictJSON(line, &fields) != nil || !exactFields(fields["command"], "action", "enrollmentId", "generation", "deadline") || strictJSON(line, &req) != nil || req.Version != 2 {
		return
	}
	c := req.Command
	now := time.Now().UnixMilli()
	if !safeID(c.EnrollmentID) || !safeID(c.Generation) || c.Deadline <= now || c.Deadline > now+10000 {
		return
	}
	ctx, cancel := context.WithDeadline(ctx, time.UnixMilli(c.Deadline))
	defer cancel()
	res := enrollmentResponse{Version: 2}
	var result enrollmentResult
	var err error
	if s.issuer == nil {
		message := "setup-required: configure private enrollment credentials; full Neige restart required after helper update"
		res.Error = &message
	} else {
		switch c.Action {
		case "create":
			result, err = s.issuer.issue(ctx, c, s)
		case "cancel":
			result, err = s.issuer.cleanup(ctx, c.EnrollmentID, false)
		case "cleanup":
			result, err = s.issuer.cleanup(ctx, "", true)
		case "status":
			s.issuer.mu.Lock()
			result.PendingCleanup = len(s.issuer.ledger.Records)
			result.Detail = cleanupDetail(s.issuer.ledger.Records)
			s.issuer.mu.Unlock()
		default:
			return
		}
		if err != nil {
			message := err.Error()
			res.Error = &message
		} else {
			result.EnrollmentID = c.EnrollmentID
			result.Generation = c.Generation
			res.Result = &result
		}
	}
	json.NewEncoder(conn).Encode(res)
}
