package main

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"tailscale.com/ipn/ipnstate"
)

type enrollmentNode struct {
	fakeNode
	locked bool
}

func TestEnrollmentCleanupOnlyNeverStartsNodeOrListener(t *testing.T) {
	build := filepath.Join(t.TempDir(), "neige-tailnet")
	command := exec.Command("go", "build", "-p", "2", "-tags", "ts_omit_logtail", "-o", build, ".")
	if output, err := command.CombinedOutput(); err != nil {
		t.Fatalf("build actual helper: %v %s", err, output)
	}
	dir := t.TempDir()
	if err := os.Chmod(dir, 0700); err != nil {
		t.Fatal(err)
	}
	// An intentionally absent configuration exercises offline cleanup. This
	// subprocess has no credentials and must not make any account request.
	command = exec.Command(build, "--state-dir", dir, "--control-socket", filepath.Join(dir, "helper.sock"), "--upstream-socket", filepath.Join(dir, "ingress.sock"), "--enrollment-config", filepath.Join(dir, "absent.json"), "--cleanup-only")
	command.Env = []string{"PATH=/usr/bin:/bin", "LANG=C.UTF-8"}
	if output, err := command.CombinedOutput(); err != nil {
		t.Fatalf("cleanup-only: %v %s", err, output)
	} else {
		var report cleanupReport
		if json.Unmarshal(output, &report) != nil || report.Version != 2 {
			t.Fatal("cleanup-only did not return its secret-free result")
		}
	}
	before, err := os.ReadFile(filepath.Join(dir, "enrollment-ledger.json"))
	if err != nil {
		t.Fatal(err)
	}
	command = exec.Command(build, "--state-dir", dir, "--control-socket", filepath.Join(dir, "helper.sock"), "--upstream-socket", filepath.Join(dir, "ingress.sock"), "--enrollment-config", filepath.Join(dir, "unreadable-config"), "--cleanup-status")
	command.Env = []string{"PATH=/usr/bin:/bin", "LANG=C.UTF-8"}
	output, err := command.CombinedOutput()
	if err != nil {
		t.Fatalf("read-only status failed: %v %s", err, output)
	}
	var report cleanupReport
	if json.Unmarshal(output, &report) != nil || report.Version != 2 || report.PendingCleanup != 0 {
		t.Fatal("invalid read-only status")
	}
	after, err := os.ReadFile(filepath.Join(dir, "enrollment-ledger.json"))
	if err != nil || string(before) != string(after) {
		t.Fatal("read-only status changed ledger")
	}
	for _, name := range []string{"node", "helper.sock", "ingress.sock", "state-version"} {
		if _, err := os.Lstat(filepath.Join(dir, name)); !os.IsNotExist(err) {
			t.Fatalf("cleanup-only created %s", name)
		}
	}
}

func (n *enrollmentNode) lockEnabled(context.Context) (bool, error) { return n.locked, nil }

type roundTripFunc func(*http.Request) (*http.Response, error)

func (f roundTripFunc) RoundTrip(r *http.Request) (*http.Response, error) { return f(r) }

func issuerFixture(t *testing.T) (*issuer, *service, enrollmentCommand, *enrollmentConfig) {
	t.Helper()
	dir := t.TempDir()
	if err := os.Chmod(dir, 0700); err != nil {
		t.Fatal(err)
	}
	c := enrollmentConfig{SchemaVersion: 1, ClientID: "fixture-client", SecretFile: filepath.Join(dir, "secret"), PhoneTags: []string{"tag:neige-phone-test"}, Tailnet: "fixture.example", Origin: "https://neige.fixture.ts.net"}
	b, _ := json.Marshal(c)
	if err := os.WriteFile(filepath.Join(dir, "config.json"), b, 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(c.SecretFile, []byte("synthetic-test-secret-only"), 0600); err != nil {
		t.Fatal(err)
	}
	i, err := newIssuer(dir, filepath.Join(dir, "config.json"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { i.dir.Close() })
	n := &enrollmentNode{fakeNode: fakeNode{state: &ipnstate.Status{BackendState: "Running", CurrentTailnet: &ipnstate.TailnetStatus{Name: c.Tailnet}, Self: &ipnstate.PeerStatus{Online: true, DNSName: "neige.fixture.ts.net."}}}}
	s := newService(n, filepath.Join(dir, "ingress.sock"))
	s.current.Origin = &c.Origin
	s.current.HTTPSReady = true
	s.current.UpstreamReady = true
	cmd := enrollmentCommand{Action: "create", EnrollmentID: "test-enrollment", Generation: "test-generation", Deadline: time.Now().Add(8 * time.Second).UnixMilli()}
	return i, s, cmd, &c
}

func responseJSON(body string, code int) *http.Response {
	return &http.Response{StatusCode: code, Header: make(http.Header), Body: io.NopCloser(strings.NewReader(body))}
}

func fixtureAPI(t *testing.T, i *issuer, c *enrollmentConfig, lifetime time.Duration, failDelete bool) *int {
	t.Helper()
	posts := new(int)
	i.api.client.Transport = roundTripFunc(func(r *http.Request) (*http.Response, error) {
		if r.URL.Scheme != "https" || r.URL.Host != "api.tailscale.com" {
			t.Fatal("non-official credential destination")
		}
		if strings.HasSuffix(r.URL.Path, "/oauth/token") {
			return responseJSON(`{"access_token":"fixture-token","token_type":"Bearer","expires_in":3600}`, 200), nil
		}
		if !strings.Contains(r.URL.Path, "/tailnet/fixture.example/keys") {
			t.Fatal("API did not bind exact tailnet", r.URL.Path)
		}
		if r.Method == "DELETE" {
			if failDelete {
				return responseJSON(`{}`, 503), nil
			}
			return responseJSON(`{}`, 404), nil
		}
		*posts++
		ledger, err := readLedger(i.dir)
		if err != nil || len(ledger.Records) != 1 || ledger.Records[0].State != "unknown" {
			t.Fatal("POST was sent before durable uncertainty marker")
		}
		var request map[string]json.RawMessage
		if json.NewDecoder(r.Body).Decode(&request) != nil || string(request["expirySeconds"]) != "300" {
			t.Fatal("not requesting 300 real seconds")
		}
		now := time.Now().UTC()
		b, _ := json.Marshal(map[string]any{"id": "real-key-id", "key": "tskey-auth-synthetic-fixture", "created": now, "expires": now.Add(lifetime), "capabilities": phoneCapabilities(c.PhoneTags)})
		return responseJSON(string(b), 200), nil
	})
	return posts
}

func TestEnrollmentValidatesRealExpiryAndRecordsRejectedKey(t *testing.T) {
	for _, ttl := range []time.Duration{300 * time.Second, 24 * time.Hour} {
		t.Run(ttl.String(), func(t *testing.T) {
			i, s, cmd, c := issuerFixture(t)
			fixtureAPI(t, i, c, ttl, true)
			result, err := i.issue(context.Background(), cmd, s)
			if ttl == 300*time.Second {
				if err != nil || result.AuthKey == "" || result.PairExpiresAt > result.AuthKeyExpiresAt || result.PairExpiresAt > time.Now().Add(180*time.Second).UnixMilli() {
					t.Fatal("valid bounded result missing", err)
				}
			} else if err == nil || result.AuthKey != "" {
				t.Fatal("cloud lifetime was replaced by a local countdown")
			}
			ledger, e := readLedger(i.dir)
			if e != nil || len(ledger.Records) != 1 || ledger.Records[0].KeyID != "real-key-id" || ledger.Records[0].Expires == "" {
				t.Fatal("returned key ID/real expiry lost", e)
			}
			b, _ := os.ReadFile(filepath.Join(i.dir.Name(), "enrollment-ledger.json"))
			if strings.Contains(string(b), "tskey-") || strings.Contains(string(b), "fixture-token") || strings.Contains(string(b), "synthetic-test-secret") {
				t.Fatal("secret persisted in cleanup ledger")
			}
		})
	}
}

func TestEnrollmentUnknownPostIsNotRetriedAcrossRestart(t *testing.T) {
	i, s, cmd, c := issuerFixture(t)
	posts := fixtureAPI(t, i, c, 300*time.Second, false)
	original := i.api.client.Transport
	i.api.client.Transport = roundTripFunc(func(r *http.Request) (*http.Response, error) {
		if r.Method == "POST" && strings.HasSuffix(r.URL.Path, "/keys") {
			*posts++
			return nil, context.DeadlineExceeded
		}
		return original.RoundTrip(r)
	})
	if _, err := i.issue(context.Background(), cmd, s); err == nil {
		t.Fatal("unknown result reported successful")
	}
	j, err := newIssuer(i.dir.Name(), i.configPath)
	if err != nil {
		t.Fatal(err)
	}
	defer j.dir.Close()
	j.api = i.api
	cmd.EnrollmentID = "replacement"
	if _, err = j.issue(context.Background(), cmd, s); err == nil || *posts != 1 {
		t.Fatal("uncertain POST blindly retried", *posts)
	}
}

func TestEnrollmentCleanupBindingRetainsOldNetworkRecords(t *testing.T) {
	i, s, cmd, c := issuerFixture(t)
	fixtureAPI(t, i, c, 300*time.Second, false)
	if _, err := i.issue(context.Background(), cmd, s); err != nil {
		t.Fatal(err)
	}
	c.Tailnet = "another-network"
	b, _ := json.Marshal(c)
	if err := os.WriteFile(i.configPath, b, 0600); err != nil {
		t.Fatal(err)
	}
	deletes := 0
	i.api.client.Transport = roundTripFunc(func(r *http.Request) (*http.Response, error) {
		if strings.HasSuffix(r.URL.Path, "/oauth/token") {
			return responseJSON(`{"access_token":"new-config-token","token_type":"Bearer","expires_in":3600}`, 200), nil
		}
		if r.Method != "DELETE" {
			t.Fatal("unexpected cleanup request")
		}
		deletes++
		return responseJSON(`{}`, 404), nil
	})
	r, err := i.cleanup(context.Background(), "", true)
	if err != nil || r.PendingCleanup != 1 || deletes != 0 {
		t.Fatal("mismatched binding was discarded", err)
	}
}

func TestEnrollmentRejectsCredentialRedirects(t *testing.T) {
	i, _, _, c := issuerFixture(t)
	calls := 0
	i.api.client.Transport = roundTripFunc(func(*http.Request) (*http.Response, error) {
		calls++
		r := responseJSON(`{}`, 307)
		r.Header.Set("Location", "https://evil.example/token")
		return r, nil
	})
	if _, err := i.api.token(context.Background(), *c); err == nil || calls != 1 {
		t.Fatal("redirect followed with credentials", calls)
	}
}

func TestEnrollmentPrivatePathsAndStrictConfig(t *testing.T) {
	i, _, _, c := issuerFixture(t)
	if _, _, err := loadEnrollmentConfig(i.configPath); err != nil {
		t.Fatal(err)
	}
	link := filepath.Join(i.dir.Name(), "link")
	if err := os.Symlink(c.SecretFile, link); err != nil {
		t.Fatal(err)
	}
	if _, err := privateRead(link, 4096); err == nil {
		t.Fatal("secret symlink followed")
	}
	if err := os.Chmod(c.SecretFile, 0640); err != nil {
		t.Fatal(err)
	}
	if _, err := privateRead(c.SecretFile, 4096); err == nil {
		t.Fatal("group-readable secret accepted")
	}
	var value enrollmentConfig
	if strictJSON([]byte(`{"schemaVersion":1,"schemaVersion":1}`), &value) == nil {
		t.Fatal("duplicate field accepted")
	}
}

func TestEnrollmentChecksActualNodeBeforeCredentials(t *testing.T) {
	for _, mode := range []string{"tailnet", "origin", "lock"} {
		t.Run(mode, func(t *testing.T) {
			i, s, cmd, _ := issuerFixture(t)
			n := s.node.(*enrollmentNode)
			switch mode {
			case "tailnet":
				n.state.CurrentTailnet.Name = "other"
			case "origin":
				n.state.Self.DNSName = "other.fixture.ts.net."
			case "lock":
				n.locked = true
			}
			i.api.client.Transport = roundTripFunc(func(*http.Request) (*http.Response, error) {
				t.Fatal("credentials sent before node validation")
				return nil, errors.New("unexpected")
			})
			if _, err := i.issue(context.Background(), cmd, s); err == nil {
				t.Fatal("incorrect node accepted")
			}
		})
	}
}

func TestEnrollmentMissingCapabilityIsNotAssumedFalse(t *testing.T) {
	i, s, cmd, c := issuerFixture(t)
	fixtureAPI(t, i, c, 300*time.Second, true)
	original := i.api.client.Transport
	i.api.client.Transport = roundTripFunc(func(r *http.Request) (*http.Response, error) {
		res, err := original.RoundTrip(r)
		if err != nil || r.Method != "POST" || !strings.HasSuffix(r.URL.Path, "/keys") {
			return res, err
		}
		var body map[string]any
		if json.NewDecoder(res.Body).Decode(&body) != nil {
			t.Fatal("fixture response")
		}
		res.Body.Close()
		delete(body["capabilities"].(map[string]any)["devices"].(map[string]any)["create"].(map[string]any), "reusable")
		bytes, _ := json.Marshal(body)
		return responseJSON(string(bytes), 200), nil
	})
	result, err := i.issue(context.Background(), cmd, s)
	if err == nil || result.AuthKey != "" {
		t.Fatal("missing reusable capability accepted as false")
	}
	ledger, err := readLedger(i.dir)
	if err != nil || len(ledger.Records) != 1 || ledger.Records[0].KeyID != "real-key-id" {
		t.Fatal("rejected key lost its cleanup ID")
	}
}

func TestEnrollmentRejectsLongActualLifetimeWithShortRemaining(t *testing.T) {
	i, s, cmd, c := issuerFixture(t)
	fixtureAPI(t, i, c, 299*time.Second, true)
	original := i.api.client.Transport
	i.api.client.Transport = roundTripFunc(func(r *http.Request) (*http.Response, error) {
		res, err := original.RoundTrip(r)
		if err != nil || r.Method != "POST" || !strings.HasSuffix(r.URL.Path, "/keys") {
			return res, err
		}
		var body map[string]any
		if json.NewDecoder(res.Body).Decode(&body) != nil {
			t.Fatal("fixture response")
		}
		res.Body.Close()
		body["created"] = time.Now().Add(-24 * time.Hour).UTC()
		bytes, _ := json.Marshal(body)
		return responseJSON(string(bytes), 200), nil
	})
	if result, err := i.issue(context.Background(), cmd, s); err == nil || result.AuthKey != "" {
		t.Fatal("short remaining time concealed a long actual key lifetime")
	}
}

func TestEnrollmentRestartPreservesLedgerAfterInterruptedTemporaryWrite(t *testing.T) {
	i, s, cmd, c := issuerFixture(t)
	fixtureAPI(t, i, c, 300*time.Second, false)
	if _, err := i.issue(context.Background(), cmd, s); err != nil {
		t.Fatal(err)
	}
	orphan := filepath.Join(i.dir.Name(), "enrollment-ledger.next")
	if err := os.WriteFile(orphan, []byte("interrupted temporary write"), 0600); err != nil {
		t.Fatal(err)
	}
	j, err := newIssuer(i.dir.Name(), i.configPath)
	if err != nil {
		t.Fatalf("interrupted temporary write blocked cleanup restart: %v", err)
	}
	defer j.dir.Close()
	if len(j.ledger.Records) != 1 || j.ledger.Records[0].KeyID != "real-key-id" || j.ledger.Records[0].State != "cleanup" {
		t.Fatal("restart lost durable cleanup record")
	}
	j.api = i.api
	if result, err := j.cleanup(context.Background(), "", true); err != nil || result.PendingCleanup != 0 {
		t.Fatal("restart did not compensate known key", err)
	}
	if b, err := os.ReadFile(orphan); err != nil || string(b) != "interrupted temporary write" {
		t.Fatal("restart overwrote unrelated temporary bytes")
	}
}

func TestEnrollmentReturnedIDMustMatchDurableMetadata(t *testing.T) {
	i, s, cmd, c := issuerFixture(t)
	fixtureAPI(t, i, c, 300*time.Second, true)
	original := i.api.client.Transport
	i.api.client.Transport = roundTripFunc(func(r *http.Request) (*http.Response, error) {
		res, err := original.RoundTrip(r)
		if err != nil || r.Method != "POST" || !strings.HasSuffix(r.URL.Path, "/keys") {
			return res, err
		}
		var body map[string]any
		if json.NewDecoder(res.Body).Decode(&body) != nil {
			t.Fatal("fixture response")
		}
		res.Body.Close()
		body["ID"] = body["id"]
		delete(body, "id")
		b, _ := json.Marshal(body)
		return responseJSON(string(b), 200), nil
	})
	if result, err := i.issue(context.Background(), cmd, s); err == nil || result.AuthKey != "" {
		t.Fatal("key published without exact durable key ID")
	}
}
