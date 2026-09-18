package main

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"reflect"
	"sort"
	"strings"
	"sync"
	"time"

	"tailscale.com/client/tailscale"
)

type enrollmentCommand struct {
	Action       string `json:"action"`
	EnrollmentID string `json:"enrollmentId"`
	Generation   string `json:"generation"`
	Deadline     int64  `json:"deadline"`
}
type enrollmentResult struct {
	EnrollmentID     string `json:"enrollmentId"`
	Generation       string `json:"generation"`
	Origin           string `json:"origin"`
	AuthKey          string `json:"authKey"`
	AuthKeyExpiresAt int64  `json:"authKeyExpiresAt"`
	PairExpiresAt    int64  `json:"pairExpiresAt"`
	PendingCleanup   int    `json:"pendingCleanup"`
	Detail           string `json:"detail"`
}

type issuer struct {
	mu         sync.Mutex
	dir        *os.File
	configPath string
	api        *enrollmentAPI
	ledger     cleanupLedger
	broken     bool
}

func newIssuer(stateDir, configPath string) (*issuer, error) {
	dir, err := privateDirectory(stateDir)
	if err != nil {
		return nil, err
	}
	l, err := readLedger(dir)
	if err != nil {
		dir.Close()
		return nil, err
	}
	i := &issuer{dir: dir, configPath: configPath, api: newEnrollmentAPI(), ledger: l}
	// Restart never republishes QR secrets or resumes a prior kernel's grant.
	for n := range i.ledger.Records {
		if i.ledger.Records[n].State == "active" {
			i.ledger.Records[n].State = "cleanup"
		}
	}
	if err = i.save(); err != nil {
		dir.Close()
		return nil, err
	}
	return i, nil
}
func (i *issuer) save() error {
	if err := writeLedger(i.dir, i.ledger); err != nil {
		i.broken = true
		return errors.New("cleanup ledger unavailable; cloud cleanup may be unknown")
	}
	return nil
}

func (i *issuer) issue(ctx context.Context, cmd enrollmentCommand, s *service) (enrollmentResult, error) {
	i.mu.Lock()
	defer i.mu.Unlock()
	result := enrollmentResult{EnrollmentID: cmd.EnrollmentID, Generation: cmd.Generation}
	if i.broken || len(i.ledger.Records) >= 64 {
		return result, errors.New("cleanup ledger requires administrator reconciliation")
	}
	for _, r := range i.ledger.Records {
		if r.State == "unknown" || r.EnrollmentID == cmd.EnrollmentID {
			return result, errors.New("key result unknown or attempt already issued; no automatic retry")
		}
	}
	c, binding, err := loadEnrollmentConfig(i.configPath)
	if err != nil {
		return result, errors.New("setup-required: configure private enrollment credentials and expected node")
	}
	if err = s.enrollmentReady(ctx, c); err != nil {
		return result, err
	}
	token, err := i.api.token(ctx, c)
	if err != nil {
		return result, err
	}
	// A crash or timeout after sending POST must not permit a blind replacement.
	i.ledger.Records = append(i.ledger.Records, cleanupRecord{EnrollmentID: cmd.EnrollmentID, BindingHash: binding, State: "unknown", Deadline: cmd.Deadline})
	index := len(i.ledger.Records) - 1
	if err = i.save(); err != nil {
		return result, err
	}
	data, err := i.api.create(ctx, c, token)
	if err != nil {
		return result, err
	}
	// Decode only cleanup metadata first. Invalid capabilities/timestamps must
	// not discard a real key ID returned by the control plane.
	var fields map[string]json.RawMessage
	if strictJSON(data, &fields) != nil {
		return result, errors.New("key result unknown; invalid response")
	}
	r := &i.ledger.Records[index]
	_ = json.Unmarshal(fields["id"], &r.KeyID)
	_ = json.Unmarshal(fields["expires"], &r.Expires)
	if !safeID(r.KeyID) {
		r.KeyID = ""
	} else {
		r.State = "cleanup"
	}
	if len(r.Expires) > 128 {
		r.Expires = ""
	}
	if err = i.save(); err != nil {
		_ = i.api.delete(ctx, c, token, r.KeyID)
		return result, err
	}
	var key struct {
		tailscale.Key
		Secret string `json:"key"`
	}
	err = json.Unmarshal(data, &key)
	now := time.Now()
	sort.Strings(key.Capabilities.Devices.Create.Tags)
	valid := err == nil && exactCapabilities(fields["capabilities"]) && safeID(key.ID) && validAuthKey(key.Secret) && reflect.DeepEqual(key.Capabilities, phoneCapabilities(c.PhoneTags)) && !key.Created.IsZero() && !key.Expires.IsZero() && !key.Created.After(now.Add(5*time.Second)) && key.Expires.After(key.Created) && key.Expires.Sub(key.Created) <= 300*time.Second && key.Expires.After(now.Add(30*time.Second)) && !key.Expires.After(now.Add(300*time.Second))
	if valid {
		err = s.enrollmentReady(ctx, c)
		valid = err == nil && ctx.Err() == nil && now.UnixMilli() < cmd.Deadline
		_, currentBinding, configErr := loadEnrollmentConfig(i.configPath)
		valid = valid && configErr == nil && currentBinding == binding
	}
	if !valid {
		if safeID(r.KeyID) && i.api.delete(ctx, c, token, r.KeyID) == nil {
			i.ledger.Records = i.ledger.Records[:index]
			_ = i.save()
		}
		return result, errors.New("setup-required: returned key capabilities, real lifetime, or current node failed validation; cleanup may remain pending")
	}
	deadline := time.Now().Add(180 * time.Second)
	if key.Expires.Before(deadline) {
		deadline = key.Expires
	}
	r.State = "active"
	r.Deadline = deadline.UnixMilli()
	if err = i.save(); err != nil {
		_ = i.api.delete(ctx, c, token, r.KeyID)
		return result, err
	}
	result.Origin = c.Origin
	result.AuthKey = key.Secret
	result.AuthKeyExpiresAt = key.Expires.UnixMilli()
	result.PairExpiresAt = deadline.UnixMilli()
	result.PendingCleanup = len(i.ledger.Records)
	result.Detail = "Short-lived key issued; real-account expiry acceptance remains required before release"
	return result, nil
}

func validAuthKey(key string) bool {
	if !strings.HasPrefix(key, "tskey-auth-") || len(key) <= len("tskey-auth-") || len(key) > 1024 {
		return false
	}
	for _, r := range key {
		if r < '!' || r > '~' {
			return false
		}
	}
	return true
}

func (i *issuer) cleanup(ctx context.Context, id string, all bool) (enrollmentResult, error) {
	i.mu.Lock()
	defer i.mu.Unlock()
	result := enrollmentResult{Detail: "Neige invitation invalidated; cloud key cleanup is not device removal"}
	if i.broken {
		return result, errors.New("cleanup ledger unavailable; administrator reconciliation required")
	}
	for n := range i.ledger.Records {
		r := &i.ledger.Records[n]
		if r.State == "active" && (all || r.EnrollmentID == id || time.Now().UnixMilli() >= r.Deadline) {
			r.State = "cleanup"
		}
	}
	if err := i.save(); err != nil {
		return result, err
	}
	c, binding, err := loadEnrollmentConfig(i.configPath)
	if err != nil {
		result.PendingCleanup = len(i.ledger.Records)
		result.Detail = "Enrollment configuration unavailable; " + cleanupDetail(i.ledger.Records)
		return result, nil
	}
	var token string
	remaining := make([]cleanupRecord, 0, len(i.ledger.Records))
	for _, r := range i.ledger.Records {
		// Never apply a new issuer's 404 (or presumed expiry) to another binding.
		if r.BindingHash != binding || r.State != "cleanup" || !safeID(r.KeyID) {
			remaining = append(remaining, r)
			continue
		}
		if token == "" {
			token, _ = i.api.token(ctx, c)
		}
		if token == "" || i.api.delete(ctx, c, token, r.KeyID) != nil {
			remaining = append(remaining, r)
		}
	}
	i.ledger.Records = remaining
	result.PendingCleanup = len(remaining)
	if result.PendingCleanup > 0 {
		result.Detail = cleanupDetail(remaining)
	}
	return result, i.save()
}

func cleanupDetail(records []cleanupRecord) string {
	detail := "Cloud key records retained; deleting a key does not remove an enrolled device"
	for _, r := range records {
		detail += "; " + r.EnrollmentID + " (" + r.State + ")"
		if expires, err := time.Parse(time.RFC3339Nano, r.Expires); err == nil {
			detail += " returned expiry " + expires.UTC().Format(time.RFC3339)
		} else {
			detail += " has unknown cloud expiry; administrator reconciliation required"
		}
	}
	return detail
}

func exactCapabilities(raw json.RawMessage) bool {
	var caps struct {
		Devices struct {
			Create struct {
				Reusable      *bool    `json:"reusable"`
				Ephemeral     *bool    `json:"ephemeral"`
				Preauthorized *bool    `json:"preauthorized"`
				Tags          []string `json:"tags"`
			} `json:"create"`
		} `json:"devices"`
	}
	return strictJSON(raw, &caps) == nil && caps.Devices.Create.Reusable != nil && caps.Devices.Create.Ephemeral != nil && caps.Devices.Create.Preauthorized != nil && caps.Devices.Create.Tags != nil
}

func (s *service) enrollmentReady(ctx context.Context, c enrollmentConfig) error {
	st := s.snapshot()
	if !st.HTTPSReady || !st.UpstreamReady || st.Origin == nil || *st.Origin != c.Origin {
		return errors.New("setup-required: private HTTPS ingress and expected origin must be ready")
	}
	node, err := s.node.status(ctx)
	if err != nil || node == nil || node.CurrentTailnet == nil || node.CurrentTailnet.Name != c.Tailnet || node.BackendState != "Running" || node.Self == nil || !node.Self.Online || "https://"+strings.TrimSuffix(node.Self.DNSName, ".") != c.Origin {
		return errors.New("setup-required: current private node does not match expected tailnet and origin")
	}
	checker, ok := s.node.(interface {
		lockEnabled(context.Context) (bool, error)
	})
	if !ok {
		return errors.New("setup-required: Tailnet Lock status unavailable")
	}
	locked, err := checker.lockEnabled(ctx)
	if err != nil || locked {
		return errors.New("setup-required: Tailnet Lock signing is not supported")
	}
	return nil
}
