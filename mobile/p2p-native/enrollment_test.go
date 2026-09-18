package main

import (
	"bufio"
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"math/big"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"tailscale.com/ipn/ipnstate"
)

type enrollmentNode struct {
	logoutErr     error
	logoutCalls   int
	authFailures  int
	registered    bool
	mu            sync.Mutex
	state         *ipnstate.Status
	running       *ipnstate.Status
	certificate   tls.Certificate
	authCalls     int
	addresses     []string
	dialEntered   chan struct{}
	dialRelease   chan struct{}
	logoutEntered chan struct{}
	logoutRelease chan struct{}
}

func (n *enrollmentNode) Status(context.Context) (*ipnstate.Status, error) {
	n.mu.Lock()
	defer n.mu.Unlock()
	return n.state, nil
}
func (n *enrollmentNode) HasIdentity(context.Context) (bool, error) { return n.registered, nil }
func (n *enrollmentNode) Logout(ctx context.Context) error {
	if n.logoutEntered != nil {
		close(n.logoutEntered)
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-n.logoutRelease:
		}
	}
	n.mu.Lock()
	defer n.mu.Unlock()
	n.logoutCalls++
	if n.logoutErr != nil {
		return n.logoutErr
	}
	n.registered = false
	n.state = &ipnstate.Status{BackendState: "NeedsLogin"}
	return nil
}
func (n *enrollmentNode) StartAuth(ctx context.Context, authKey string) error {
	n.mu.Lock()
	defer n.mu.Unlock()
	if ctx.Err() != nil {
		return ctx.Err()
	}
	if authKey != "tskey-auth-native-only-secret" {
		return errors.New("bad fixture input")
	}
	n.authCalls++
	if n.authFailures > 0 {
		n.authFailures--
		return errors.New("transient control failure")
	}
	n.state = n.running
	return nil
}
func (n *enrollmentNode) Dial(ctx context.Context, network, address string) (net.Conn, error) {
	n.mu.Lock()
	n.addresses = append(n.addresses, address)
	n.mu.Unlock()
	if n.dialEntered != nil {
		close(n.dialEntered)
		select {
		case <-ctx.Done():
			return nil, ctx.Err()
		case <-n.dialRelease:
		}
	}
	left, right := net.Pipe()
	go func() {
		defer right.Close()
		server := tls.Server(right, &tls.Config{Certificates: []tls.Certificate{n.certificate}, MinVersion: tls.VersionTLS12})
		_ = server.Handshake()
	}()
	return left, nil
}
func enrollmentFixture(t *testing.T, running bool) (*engine, *enrollmentNode, string) {
	t.Helper()
	pub, key, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	cert := &x509.Certificate{SerialNumber: big.NewInt(1), Subject: pkix.Name{CommonName: "test CA"}, DNSNames: []string{"alpha.tail.example"},
		NotBefore: time.Now().Add(-time.Hour), NotAfter: time.Now().Add(time.Hour), IsCA: true, BasicConstraintsValid: true,
		KeyUsage: x509.KeyUsageCertSign | x509.KeyUsageDigitalSignature, ExtKeyUsage: []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth}}
	der, err := x509.CreateCertificate(rand.Reader, cert, cert, pub, key)
	if err != nil {
		t.Fatal(err)
	}
	parsed, err := x509.ParseCertificate(der)
	if err != nil {
		t.Fatal(err)
	}
	roots := x509.NewCertPool()
	roots.AddCert(parsed)
	node := &enrollmentNode{running: targetStatus(t, targetPeer()), certificate: tls.Certificate{Certificate: [][]byte{der}, PrivateKey: key}}
	node.state = &ipnstate.Status{BackendState: "NeedsLogin"}
	if running {
		node.state = node.running
	}
	e := &engine{dir: t.TempDir(), transport: node, tlsRoots: roots}
	t.Cleanup(e.closeTunnel)
	fields := enrollmentTestFields()
	fields["authKeyExpiresAt"] = time.Now().Add(300 * time.Second).UnixMilli()
	fields["pairExpiresAt"] = time.Now().Add(180 * time.Second).UnixMilli()
	return e, node, enrollmentTestQR(t, fields)
}

func TestEnrollmentProductionFlowUsesOneNodeAndClearsSecretsBeforeHandoff(t *testing.T) {
	e, node, qr := enrollmentFixture(t, false)
	result, err := e.enroll(qr)
	if err != nil {
		t.Fatal(err)
	}
	if node.authCalls != 1 || len(node.addresses) != 1 || node.addresses[0] != "100.100.1.2:10000" {
		t.Fatal("enrollment did not use the existing numeric node transport")
	}
	if strings.Contains(string(result), "tskey-auth-") {
		t.Fatal("provider key escaped native owner")
	}
	if _, err := os.Stat(filepath.Join(e.dir, pendingFilename)); !os.IsNotExist(err) {
		t.Fatal("handoff retained pending secrets")
	}
	var decoded struct {
		Bootstrap struct{ AttemptID, AttemptSecret, PairTicket string }
		Proxy     string
	}
	if json.Unmarshal(result, &decoded) != nil || decoded.Bootstrap.AttemptID == "" || len(decoded.Bootstrap.AttemptSecret) != 64 || decoded.Proxy == "" {
		t.Fatal("missing one-document context")
	}
	if _, err := savedTarget(e.dir, "https://alpha.tail.example:10000"); err != nil {
		t.Fatal(err)
	}
}

func TestEnrollmentExistingRunningIdentitySkipsKeyAndPreservesOldTarget(t *testing.T) {
	e, node, qr := enrollmentFixture(t, true)
	old := tailnetBinding{SchemaVersion: 1, Origin: "https://old.tail.example", PeerID: "old", TailnetIP: targetPeer().TailscaleIPs[0]}
	if err := saveTarget(e.dir, old); err != nil {
		t.Fatal(err)
	}
	if _, err := e.enroll(qr); err != nil {
		t.Fatal(err)
	}
	if node.authCalls != 0 {
		t.Fatal("existing node used a new key")
	}
	if restored, err := savedTarget(e.dir, old.Origin); err != nil || restored != old {
		t.Fatal("old profile was overwritten")
	}
}

func TestEnrollmentRejectsExpiredOrUnknownExistingIdentityWithoutAuth(t *testing.T) {
	for _, source := range []string{"disk", "self", "unknown-attempt"} {
		t.Run(source, func(t *testing.T) {
			e, node, qr := enrollmentFixture(t, false)
			switch source {
			case "disk":
				path := filepath.Join(e.dir, "tailscaled.state")
				if err := os.WriteFile(path, []byte("existing opaque node identity"), 0600); err != nil {
					t.Fatal(err)
				}
				node.registered = true
			case "self":
				node.state.Self = &ipnstate.PeerStatus{ID: "old-expired-node", Expired: true}
			case "unknown-attempt":
				e.identityClaimed.Store(true)
			}
			if _, err := e.enroll(qr); err == nil || node.authCalls != 0 {
				t.Fatal("replaced an old or unknown identity")
			}
			if source == "disk" {
				if _, err := os.Stat(filepath.Join(e.dir, "tailscaled.state")); err != nil {
					t.Fatal("deleted existing identity")
				}
			}
		})
	}
}

func TestEnrollmentWrongPeerOrTLSNeverCommitsCandidate(t *testing.T) {
	for _, mode := range []string{"missing", "certificate"} {
		t.Run(mode, func(t *testing.T) {
			e, node, qr := enrollmentFixture(t, true)
			if mode == "missing" {
				node.state = targetStatus(t)
			} else {
				e.tlsRoots = x509.NewCertPool()
			}
			if _, err := e.enroll(qr); err == nil {
				t.Fatal("untrusted target admitted")
			}
			if _, err := savedTarget(e.dir, "https://alpha.tail.example:10000"); err == nil {
				t.Fatal("failed candidate committed")
			}
			if node.authCalls != 0 {
				t.Fatal("foreign identity silently changed")
			}
		})
	}
}

func TestEnrollmentCancelDuringTLSClosesPermitAndDiscardsLateHandoff(t *testing.T) {
	e, node, qr := enrollmentFixture(t, true)
	node.dialEntered = make(chan struct{})
	node.dialRelease = make(chan struct{})
	done := make(chan error, 1)
	go func() { _, err := e.enroll(qr); done <- err }()
	select {
	case <-node.dialEntered:
	case <-time.After(time.Second):
		t.Fatal("TLS stage not entered")
	}
	if err := e.cancelEnrollment(); err != nil {
		t.Fatal(err)
	}
	close(node.dialRelease)
	select {
	case err := <-done:
		if err == nil {
			t.Fatal("cancelled handoff accepted")
		}
	case <-time.After(time.Second):
		t.Fatal("cancel did not release operation")
	}
	if e.tunnel != nil {
		t.Fatal("cancelled operation installed proxy")
	}
}

func TestTailnetReplacementClosesExistingConnectionsAndNeverReusesPort(t *testing.T) {
	e, _, qr := enrollmentFixture(t, true)
	if _, err := e.enroll(qr); err != nil {
		t.Fatal(err)
	}
	old := e.tunnel
	left, right := net.Pipe()
	defer right.Close()
	if !old.track(left) {
		t.Fatal("proxy already closed")
	}
	if _, err := e.enroll(qr); err != nil {
		t.Fatal(err)
	}
	if old.ctx.Err() == nil || old.proxyURL() == e.tunnel.proxyURL() {
		t.Fatal("old document could acquire new destination")
	}
	_ = right.SetReadDeadline(time.Now().Add(time.Second))
	if _, err := right.Read(make([]byte, 1)); err == nil {
		t.Fatal("old socket survived")
	}
	if _, err := old.dial(context.Background(), "tcp", old.authority); err == nil {
		t.Fatal("retired proxy dialed")
	}
}

func TestEnrollmentPendingStorageFailureNeverStartsAuth(t *testing.T) {
	e, node, qr := enrollmentFixture(t, false)
	if err := os.Mkdir(filepath.Join(e.dir, pendingFilename), 0700); err != nil {
		t.Fatal(err)
	}
	if _, err := e.enroll(qr); err == nil || node.authCalls != 0 {
		t.Fatal("auth started without durable pending state")
	}
}

func TestTailnetProxyProductionConnectUsesValidatedNumericPeer(t *testing.T) {
	e, node, qr := enrollmentFixture(t, true)
	if _, err := e.enroll(qr); err != nil {
		t.Fatal(err)
	}
	connection, err := net.DialTimeout("tcp", e.tunnel.listener.Addr().String(), time.Second)
	if err != nil {
		t.Fatal(err)
	}
	defer connection.Close()
	connection.SetDeadline(time.Now().Add(2 * time.Second))
	fmt.Fprintf(connection, "CONNECT alpha.tail.example:10000 HTTP/1.1\r\nHost: alpha.tail.example:10000\r\n\r\n")
	response, err := http.ReadResponse(bufio.NewReader(connection), nil)
	if err != nil {
		t.Fatal(err)
	}
	if response.StatusCode != 200 {
		t.Fatalf("CONNECT status %d", response.StatusCode)
	}
	secure := tls.Client(connection, &tls.Config{ServerName: "alpha.tail.example", RootCAs: e.tlsRoots, MinVersion: tls.VersionTLS12})
	if err := secure.Handshake(); err != nil {
		t.Fatal(err)
	}
	node.mu.Lock()
	defer node.mu.Unlock()
	if len(node.addresses) != 2 || node.addresses[1] != "100.100.1.2:10000" {
		t.Fatalf("CONNECT bypassed numeric binding: %v", node.addresses)
	}
}

func TestEnrollmentUnknownRegistrationSurvivesProcessOwnerReplacement(t *testing.T) {
	e, node, qr := enrollmentFixture(t, false)
	if err := writePrivateJSON(e.dir, "registration-attempt.json", struct {
		Attempted bool `json:"attempted"`
	}{true}); err != nil {
		t.Fatal(err)
	}
	// A new engine cannot turn an uncertain prior result into permission to join
	// another tailnet, even while the local API still says NeedsLogin.
	replacement := &engine{dir: e.dir, transport: node, tlsRoots: e.tlsRoots}
	if _, err := replacement.enroll(qr); err == nil || node.authCalls != 0 {
		t.Fatal("replaced an uncertain prior identity after process death")
	}
}

func TestTailnetNetworkLossClosesSocketsButRetainsSameTargetRetry(t *testing.T) {
	e, _, qr := enrollmentFixture(t, true)
	if _, err := e.enroll(qr); err != nil {
		t.Fatal(err)
	}
	tunnel := e.tunnel
	left, right := net.Pipe()
	defer right.Close()
	tunnel.track(left)
	old := tunnel.networkContext()
	tunnel.invalidateNetwork()
	if old.Err() == nil || tunnel.ctx.Err() != nil {
		t.Fatal("network loss did not preserve document retry ownership")
	}
	if _, err := right.Read(make([]byte, 1)); err != io.EOF {
		t.Fatalf("old socket remained open: %v", err)
	}
	connection, err := tunnel.dial(tunnel.networkContext(), "tcp", tunnel.authority)
	if err != nil {
		t.Fatal(err)
	}
	tunnel.forget(connection)
}

type dialCheckpointContext struct {
	context.Context
	once      sync.Once
	inspected chan struct{}
	resume    chan struct{}
}

func (c *dialCheckpointContext) Err() error {
	observed := c.Context.Err()
	c.once.Do(func() { close(c.inspected); <-c.resume })
	return observed
}
func TestTailnetDialCannotRegisterAfterItsNetworkGenerationWasInvalidated(t *testing.T) {
	e, _, qr := enrollmentFixture(t, true)
	if _, err := e.enroll(qr); err != nil {
		t.Fatal(err)
	}
	tunnel := e.tunnel
	ctx := &dialCheckpointContext{Context: tunnel.networkContext(), inspected: make(chan struct{}), resume: make(chan struct{})}
	result := make(chan error, 1)
	go func() {
		connection, err := tunnel.dial(ctx, "tcp", tunnel.authority)
		if connection != nil {
			connection.Close()
		}
		result <- err
	}()
	select {
	case <-ctx.inspected:
	case <-time.After(time.Second):
		t.Fatal("dial did not reach the admission checkpoint")
	}
	tunnel.invalidateNetwork()
	close(ctx.resume)
	select {
	case err := <-result:
		if err == nil {
			t.Fatal("retired network generation registered a late dial")
		}
	case <-time.After(time.Second):
		t.Fatal("late dial did not settle")
	}
}

func TestEnrollmentRetriesOnlyTheSamePendingInvitationAfterTransientAuthFailure(t *testing.T) {
	for _, restart := range []bool{false, true} {
		t.Run(fmt.Sprint(restart), func(t *testing.T) {
			e, node, qr := enrollmentFixture(t, false)
			node.authFailures = 1
			if _, err := e.enroll(qr); err == nil {
				t.Fatal("expected first network failure")
			}
			record, err := os.ReadFile(filepath.Join(e.dir, "registration-attempt.json"))
			if err != nil || strings.Contains(string(record), "tskey-auth-") || strings.Contains(string(record), strings.Repeat("a", 64)) {
				t.Fatal("uncertain registration retained a bearer credential")
			}
			owner := e
			if restart {
				owner = &engine{dir: e.dir, transport: node, tlsRoots: e.tlsRoots}
				t.Cleanup(owner.closeTunnel)
			}
			if _, err := owner.enroll(qr); err != nil {
				t.Fatalf("same valid invitation could not resume: %v", err)
			}
			if node.authCalls != 2 {
				t.Fatalf("retry calls=%d", node.authCalls)
			}
		})
	}
}

func TestEnrollmentDifferentPendingInvitationCannotReplaceUnknownIdentity(t *testing.T) {
	for _, field := range []string{"enrollmentId", "origin", "authKey", "pairTicket", "authKeyExpiresAt", "pairExpiresAt"} {
		t.Run(field, func(t *testing.T) {
			e, node, qr := enrollmentFixture(t, false)
			node.authFailures = 1
			if _, err := e.enroll(qr); err == nil {
				t.Fatal("expected uncertain registration")
			}
			payload, err := decodeEnrollmentPayload(qr, time.Now())
			if err != nil {
				t.Fatal(err)
			}
			fields := enrollmentTestFields()
			fields["authKeyExpiresAt"], fields["pairExpiresAt"] = payload.authKeyExpiresAt, payload.pairExpiresAt
			switch field {
			case "enrollmentId":
				fields[field] = "other-invitation"
			case "origin":
				fields[field] = "https://other.tail.example"
			case "authKey":
				fields[field] = "tskey-auth-other-credential"
			case "pairTicket":
				fields[field] = strings.Repeat("c", 64)
			case "authKeyExpiresAt":
				fields[field] = payload.authKeyExpiresAt - 1000
			case "pairExpiresAt":
				fields[field] = payload.pairExpiresAt - 1000
			}
			replacement := &engine{dir: e.dir, transport: node, tlsRoots: e.tlsRoots}
			if _, err := replacement.enroll(enrollmentTestQR(t, fields)); err == nil || node.authCalls != 1 {
				t.Fatal("different QR replaced unknown identity")
			}
			if _, err := replacement.enroll(qr); err != nil {
				t.Fatalf("rejection damaged original retry: %v", err)
			}
			replacement.closeTunnel()
		})
	}
}

func TestExplicitEnrollmentResetWaitsForSDKLogoutAndPreservesNodeDirectory(t *testing.T) {
	e, node, qr := enrollmentFixture(t, false)
	node.authFailures = 1
	if _, err := e.enroll(qr); err == nil {
		t.Fatal("expected uncertain enrollment")
	}
	path := filepath.Join(e.dir, "tailscaled.state")
	if err := os.WriteFile(path, []byte("opaque SDK state"), 0600); err != nil {
		t.Fatal(err)
	}
	node.logoutErr = errors.New("offline")
	if err := e.resetEnrollment(); err == nil {
		t.Fatal("failed logout was accepted")
	}
	if _, err := os.Stat(filepath.Join(e.dir, "registration-attempt.json")); err != nil {
		t.Fatal("failure cleared uncertainty fence")
	}
	node.logoutErr = nil
	if err := e.resetEnrollment(); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(path); err != nil {
		t.Fatal("reset removed SDK state")
	}
	if node.logoutCalls != 2 || e.identityClaimed.Load() {
		t.Fatal("reset did not follow SDK confirmation")
	}
	if _, err := e.enroll(qr); err != nil {
		t.Fatalf("explicit reset did not restore scan entry: %v", err)
	}
}

func TestResetEnrollmentPreventsRebindingDuringAndAfterLogout(t *testing.T) {
	e, node, qr := enrollmentFixture(t, true)
	if _, err := e.enroll(qr); err != nil {
		t.Fatal(err)
	}
	node.logoutEntered, node.logoutRelease = make(chan struct{}), make(chan struct{})
	done := make(chan error, 1)
	go func() { done <- e.resetEnrollment() }()
	select {
	case <-node.logoutEntered:
	case <-time.After(time.Second):
		t.Fatal("logout not entered")
	}
	_, bindErr := e.bindTailnet("https://alpha.tail.example:10000")
	close(node.logoutRelease)
	if err := <-done; err != nil {
		t.Fatal(err)
	}
	if bindErr == nil {
		t.Fatal("rebound a retired target during logout")
	}
	if _, err := e.bindTailnet("https://alpha.tail.example:10000"); err == nil {
		t.Fatal("reset retained an old identity's target")
	}
}

func TestFailedResetRemainsBlockedAfterProcessReplacementUntilConfirmedRetry(t *testing.T) {
	e, node, qr := enrollmentFixture(t, true)
	if _, err := e.enroll(qr); err != nil {
		t.Fatal(err)
	}
	node.logoutErr = errors.New("unknown logout result")
	if err := e.resetEnrollment(); err == nil {
		t.Fatal("expected failed logout")
	}
	// The result may have reached control even though its reply was lost.
	node.state = &ipnstate.Status{BackendState: "NeedsLogin"}
	replacement := &engine{dir: e.dir, transport: node, tlsRoots: e.tlsRoots}
	t.Cleanup(replacement.closeTunnel)
	_, scanErr := replacement.enroll(qr)
	if scanErr == nil || node.authCalls != 0 {
		t.Fatal("unknown reset result permitted implicit re-enrollment")
	}
	node.logoutErr = nil
	if err := replacement.resetEnrollment(); err != nil {
		t.Fatal(err)
	}
	if _, err := replacement.enroll(qr); err != nil {
		t.Fatalf("confirmed reset did not release scan: %v", err)
	}
}
