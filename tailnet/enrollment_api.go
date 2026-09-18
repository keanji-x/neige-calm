package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"net/url"
	"strings"
	"time"

	"tailscale.com/client/tailscale"
)

const officialAPI = "https://api.tailscale.com/api/v2"

type enrollmentAPI struct{ client *http.Client }

func newEnrollmentAPI() *enrollmentAPI {
	return &enrollmentAPI{client: &http.Client{
		Timeout:       8 * time.Second,
		Transport:     &http.Transport{TLSHandshakeTimeout: 4 * time.Second, ResponseHeaderTimeout: 6 * time.Second},
		CheckRedirect: func(*http.Request, []*http.Request) error { return errors.New("credential redirects forbidden") },
	}}
}

func (a *enrollmentAPI) token(ctx context.Context, c enrollmentConfig) (string, error) {
	secret, err := privateRead(c.SecretFile, 4096)
	if err != nil {
		return "", errors.New("setup-required: private OAuth secret unavailable")
	}
	defer clear(secret)
	s := strings.TrimSpace(string(secret))
	if len(s) < 16 || strings.ContainsAny(s, "\r\n\x00") {
		return "", errors.New("setup-required: invalid OAuth secret")
	}
	form := url.Values{"grant_type": {"client_credentials"}, "client_id": {c.ClientID}, "client_secret": {s}, "scope": {"auth_keys"}}
	req, err := http.NewRequestWithContext(ctx, "POST", officialAPI+"/oauth/token", strings.NewReader(form.Encode()))
	if err != nil {
		return "", errors.New("OAuth request unavailable")
	}
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	res, err := a.client.Do(req)
	if err != nil {
		return "", errors.New("setup-required: OAuth unavailable")
	}
	defer res.Body.Close()
	data, err := io.ReadAll(io.LimitReader(res.Body, 16385))
	if err != nil || len(data) > 16384 || res.StatusCode != 200 {
		return "", errors.New("setup-required: OAuth scope or credentials rejected")
	}
	var token struct {
		AccessToken string `json:"access_token"`
		TokenType   string `json:"token_type"`
		ExpiresIn   int64  `json:"expires_in"`
	}
	if json.Unmarshal(data, &token) != nil || token.TokenType != "Bearer" && token.TokenType != "bearer" || token.ExpiresIn <= 10 || token.AccessToken == "" || len(token.AccessToken) > 8192 {
		return "", errors.New("setup-required: invalid OAuth response")
	}
	// Token stays within this one bounded operation, never on disk or the wire
	// control channel. No automatic refresh can replay a key POST.
	return token.AccessToken, nil
}

func (a *enrollmentAPI) create(ctx context.Context, c enrollmentConfig, token string) ([]byte, error) {
	body, _ := json.Marshal(struct {
		Capabilities tailscale.KeyCapabilities `json:"capabilities"`
		Expiry       int                       `json:"expirySeconds"`
	}{phoneCapabilities(c.PhoneTags), 300})
	req, _ := http.NewRequestWithContext(ctx, "POST", officialAPI+"/tailnet/"+url.PathEscape(c.Tailnet)+"/keys", bytes.NewReader(body))
	req.Header.Set("Authorization", "Bearer "+token)
	req.Header.Set("Content-Type", "application/json")
	res, err := a.client.Do(req)
	if err != nil {
		return nil, errors.New("key creation result unknown; administrator must reconcile before another attempt")
	}
	defer res.Body.Close()
	data, err := io.ReadAll(io.LimitReader(res.Body, 16385))
	if err != nil || len(data) > 16384 {
		return nil, errors.New("key creation result unknown; response unavailable")
	}
	if res.StatusCode != 200 {
		return nil, errors.New("key creation rejected or unknown; administrator must reconcile")
	}
	return data, nil
}

func phoneCapabilities(tags []string) tailscale.KeyCapabilities {
	return tailscale.KeyCapabilities{Devices: tailscale.KeyDeviceCapabilities{Create: tailscale.KeyDeviceCreateCapabilities{Reusable: false, Ephemeral: false, Preauthorized: true, Tags: tags}}}
}

func (a *enrollmentAPI) delete(ctx context.Context, c enrollmentConfig, token, id string) error {
	if !safeID(id) {
		return errors.New("unknown key ID requires administrator reconciliation")
	}
	req, _ := http.NewRequestWithContext(ctx, "DELETE", officialAPI+"/tailnet/"+url.PathEscape(c.Tailnet)+"/keys/"+url.PathEscape(id), nil)
	req.Header.Set("Authorization", "Bearer "+token)
	res, err := a.client.Do(req)
	if err != nil {
		return errors.New("cloud key cleanup unconfirmed")
	}
	defer res.Body.Close()
	if res.StatusCode != 200 && res.StatusCode != 204 && res.StatusCode != 404 {
		return errors.New("cloud key cleanup unconfirmed")
	}
	return nil
}
