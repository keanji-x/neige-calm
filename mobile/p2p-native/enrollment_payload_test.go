package main

import (
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"strings"
	"testing"
	"time"
)

var enrollmentTestNow = time.UnixMilli(1_800_000_000_000)

func enrollmentTestFields() map[string]any {
	return map[string]any{
		"version": 2, "enrollmentId": "invitation-123", "origin": "https://alpha.tail.example:10000",
		"authKey": "tskey-auth-native-only-secret", "authKeyExpiresAt": enrollmentTestNow.UnixMilli() + 300_000,
		"pairTicket": strings.Repeat("ab", 32), "pairExpiresAt": enrollmentTestNow.UnixMilli() + 180_000,
	}
}

func enrollmentTestQR(t *testing.T, fields map[string]any) string {
	t.Helper()
	raw, err := json.Marshal(fields)
	if err != nil {
		t.Fatal(err)
	}
	return enrollmentTestData(raw)
}

func enrollmentTestData(raw []byte) string {
	return "neige-enroll:v2:" + base64.RawURLEncoding.EncodeToString(raw)
}

func TestEnrollmentPayloadDecodesNativeFields(t *testing.T) {
	fields := enrollmentTestFields()
	payload, err := decodeEnrollmentPayload(enrollmentTestQR(t, fields), enrollmentTestNow)
	if err != nil {
		t.Fatal(err)
	}
	if payload.enrollmentID != fields["enrollmentId"] || payload.origin != fields["origin"] ||
		payload.nativeAuthKey() != fields["authKey"] || payload.pairTicket != fields["pairTicket"] ||
		payload.authKeyExpiresAt != fields["authKeyExpiresAt"] || payload.pairExpiresAt != fields["pairExpiresAt"] {
		t.Fatal("native envelope lost a required field")
	}
	// Provider keys remain opaque after their credential-family prefix.
	for _, key := range []string{"tskey-auth-x", "tskey-auth-a/b+c=._:!?", "tskey-auth-" + strings.Repeat("a", 400)} {
		fields["authKey"] = key
		if _, err := decodeEnrollmentPayload(enrollmentTestQR(t, fields), enrollmentTestNow); err != nil {
			t.Fatal("rejected an opaque auth-family key")
		}
	}
	fields["enrollmentId"] = strings.Repeat("a", 128)
	fields["authKey"] = "tskey-auth-" + strings.Repeat("a", 1013)
	if _, err := decodeEnrollmentPayload(enrollmentTestQR(t, fields), enrollmentTestNow); err != nil {
		t.Fatal("rejected maximum field lengths within the QR limit")
	}
}

func TestEnrollmentPayloadRejectsDuplicateFields(t *testing.T) {
	fields := enrollmentTestFields()
	body, _ := json.Marshal(fields)
	for name, value := range fields {
		entry, _ := json.Marshal(map[string]any{name: value})
		duplicate := append(append([]byte{}, body[:len(body)-1]...), ',')
		duplicate = append(duplicate, entry[1:]...)
		if _, err := decodeEnrollmentPayload(enrollmentTestData(duplicate), enrollmentTestNow); err == nil {
			t.Errorf("accepted duplicate %s", name)
		}
	}
	escaped := strings.TrimSuffix(string(body), "}") + `,"enrollment\u0049d":"invitation-123"}`
	if _, err := decodeEnrollmentPayload(enrollmentTestData([]byte(escaped)), enrollmentTestNow); err == nil {
		t.Fatal("accepted escaped duplicate key")
	}
}

func TestEnrollmentPayloadRejectsCredentialFamilies(t *testing.T) {
	for _, key := range []string{"tskey-client-OAuth-secret", "tskey-api-API-secret", "tskey-oauth-client-secret",
		"Bearer tskey-auth-not-a-key", "sk-proj-long-lived-secret", "TSKEY-AUTH-uppercase-secret", "not-a-provider-credential"} {
		fields := enrollmentTestFields()
		fields["authKey"] = key
		if _, err := decodeEnrollmentPayload(enrollmentTestQR(t, fields), enrollmentTestNow); err == nil {
			t.Error("accepted a non-auth credential family")
		}
	}
}

func TestEnrollmentPayloadRejectsUnsafeOrigins(t *testing.T) {
	for _, origin := range []string{"http://alpha.tail.example", "https://user:secret@alpha.tail.example", "https://127.0.0.1",
		"https://100.100.1.2", "https://[::1]", "https://localhost", "https://ALPHA.tail.example", "https://alpha.tail.example.",
		"https://alpha.tail.example:443", "https://alpha.tail.example:0", "https://alpha.tail.example:65536",
		"https://alpha.tail.example/next/", "https://alpha.tail.example?callback=evil", "https://alpha.tail.example#secret"} {
		fields := enrollmentTestFields()
		fields["origin"] = origin
		if _, err := decodeEnrollmentPayload(enrollmentTestQR(t, fields), enrollmentTestNow); err == nil {
			t.Errorf("accepted unsafe origin %q", origin)
		}
	}
}

func TestEnrollmentPayloadRejectsMissingUnknownAndWrongTypeFields(t *testing.T) {
	for name := range enrollmentTestFields() {
		fields := enrollmentTestFields()
		delete(fields, name)
		if _, err := decodeEnrollmentPayload(enrollmentTestQR(t, fields), enrollmentTestNow); err == nil {
			t.Errorf("accepted missing %s", name)
		}
		for _, value := range []any{nil, true, []any{}, map[string]any{}, 2.5} {
			fields := enrollmentTestFields()
			fields[name] = value
			if _, err := decodeEnrollmentPayload(enrollmentTestQR(t, fields), enrollmentTestNow); err == nil {
				t.Errorf("accepted wrong type for %s", name)
			}
		}
	}
	for _, name := range []string{"Version", "peerId", "tailnetIp", "callbackURL", "oauthSecret", "sessionId", "extra"} {
		fields := enrollmentTestFields()
		fields[name] = "private-injected-value"
		if _, err := decodeEnrollmentPayload(enrollmentTestQR(t, fields), enrollmentTestNow); err == nil {
			t.Errorf("accepted unknown %s", name)
		}
	}
	for _, version := range []any{0, 1, 3, "2", json.Number("2.0"), json.Number("2e0")} {
		fields := enrollmentTestFields()
		fields["version"] = version
		if _, err := decodeEnrollmentPayload(enrollmentTestQR(t, fields), enrollmentTestNow); err == nil {
			t.Error("accepted a non-integer-v2 version")
		}
	}
}

func TestEnrollmentPayloadRejectsMalformedEncodingAndJSON(t *testing.T) {
	qr := enrollmentTestQR(t, enrollmentTestFields())
	for _, raw := range []string{"", strings.Replace(qr, ":v2:", ":v1:", 1), "https://example.com/mobile/pair#v1.secret",
		qr + "=", qr + "\n", "neige-enroll:v2:+w", "neige-enroll:v2:/w", "neige-enroll:v2:A", " " + qr} {
		if _, err := decodeEnrollmentPayload(raw, enrollmentTestNow); err == nil {
			t.Error("accepted invalid or noncanonical data encoding")
		}
	}
	body, _ := json.Marshal(enrollmentTestFields())
	for len(body)%3 == 0 {
		body = append(body, ' ')
	}
	encoded := base64.RawURLEncoding.EncodeToString(body)
	const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
	last := strings.IndexByte(alphabet, encoded[len(encoded)-1])
	noncanonical := "neige-enroll:v2:" + encoded[:len(encoded)-1] + string(alphabet[last|1])
	if _, err := decodeEnrollmentPayload(noncanonical, enrollmentTestNow); err == nil {
		t.Fatal("accepted nonzero base64 padding bits")
	}
	for _, body := range [][]byte{[]byte("null"), []byte("[]"), []byte(`"string"`), []byte(`{"version":2,}`),
		[]byte(`{"version":2} {}`), []byte("\xef\xbb\xbf{}"), []byte(`{"version":2/*comment*/}`),
		[]byte(strings.Replace(string(body), "invitation-123", "bad\xffutf8", 1)),
		[]byte(strings.Replace(string(body), "invitation-123", `bad\ud800`, 1)),
		[]byte(strings.Replace(string(body), "invitation-123", `bad\udfff`, 1))} {
		if _, err := decodeEnrollmentPayload(enrollmentTestData(body), enrollmentTestNow); err == nil {
			t.Error("accepted malformed, non-object or non-UTF8 JSON")
		}
	}
}

func TestEnrollmentPayloadEnforcesSizeAndFieldBounds(t *testing.T) {
	fields := enrollmentTestFields()
	body, _ := json.Marshal(fields)
	// JSON whitespace is legal; only its base64url representation is canonical.
	body = append(body, []byte(strings.Repeat(" ", 1524-len(body)))...)
	qr := enrollmentTestData(body)
	if len(qr) != 2048 {
		t.Fatal("incorrect boundary fixture")
	}
	if _, err := decodeEnrollmentPayload(qr, enrollmentTestNow); err != nil {
		t.Fatal("rejected the maximum permitted QR size")
	}
	if _, err := decodeEnrollmentPayload(enrollmentTestData(append(body, ' ')), enrollmentTestNow); err == nil {
		t.Fatal("accepted an oversized QR")
	}
	for _, tc := range []struct{ name, value string }{
		{"enrollmentId", ""}, {"enrollmentId", strings.Repeat("a", 129)}, {"enrollmentId", "bad id"},
		{"origin", ""}, {"origin", "https://" + strings.Repeat("a", 505)},
		{"authKey", ""}, {"authKey", "tskey-auth-"}, {"authKey", "tskey-auth-\tsecret"},
		{"authKey", "tskey-auth-\x00secret"}, {"authKey", "tskey-auth-é"}, {"authKey", "tskey-auth-" + strings.Repeat("a", 1014)},
		{"pairTicket", ""}, {"pairTicket", strings.Repeat("a", 63)}, {"pairTicket", strings.Repeat("a", 65)},
		{"pairTicket", strings.Repeat("A", 64)}, {"pairTicket", strings.Repeat("g", 64)}, {"pairTicket", "bad\nticket"},
	} {
		fields := enrollmentTestFields()
		fields[tc.name] = tc.value
		if _, err := decodeEnrollmentPayload(enrollmentTestQR(t, fields), enrollmentTestNow); err == nil {
			t.Errorf("accepted invalid %s bounds", tc.name)
		}
	}
}

func TestEnrollmentPayloadChecksLocalTimeBounds(t *testing.T) {
	for _, name := range []string{"authKeyExpiresAt", "pairExpiresAt"} {
		for _, value := range []any{0, -1, enrollmentTestNow.UnixMilli(), enrollmentTestNow.Unix(), "1800000300000",
			json.Number("1800000300000.0"), json.Number("18e11"), json.Number("9223372036854775808"),
			int64(253402300800000), enrollmentTestNow.UnixMilli() + 360_001} {
			fields := enrollmentTestFields()
			fields[name] = value
			if _, err := decodeEnrollmentPayload(enrollmentTestQR(t, fields), enrollmentTestNow); err == nil {
				t.Errorf("accepted invalid %s", name)
			}
		}
	}
	fields := enrollmentTestFields()
	fields["pairExpiresAt"] = enrollmentTestNow.UnixMilli() + 240_001
	if _, err := decodeEnrollmentPayload(enrollmentTestQR(t, fields), enrollmentTestNow); err == nil {
		t.Fatal("accepted pair expiry over 180 seconds plus clock margin")
	}
	fields["authKeyExpiresAt"] = enrollmentTestNow.UnixMilli() + 360_000
	fields["pairExpiresAt"] = enrollmentTestNow.UnixMilli() + 240_000
	if _, err := decodeEnrollmentPayload(enrollmentTestQR(t, fields), enrollmentTestNow); err != nil {
		t.Fatal("rejected the documented 60-second clock margin")
	}
	fields["authKeyExpiresAt"] = enrollmentTestNow.UnixMilli() + 1
	fields["pairExpiresAt"] = enrollmentTestNow.UnixMilli() + 2
	if _, err := decodeEnrollmentPayload(enrollmentTestQR(t, fields), enrollmentTestNow); err == nil {
		t.Fatal("pair deadline exceeded key deadline")
	}
	fields["pairExpiresAt"] = enrollmentTestNow.UnixMilli() + 1
	if _, err := decodeEnrollmentPayload(enrollmentTestQR(t, fields), enrollmentTestNow.In(time.FixedZone("different-zone", 28800))); err != nil {
		t.Fatal("UTC milliseconds depended on phone timezone")
	}
	epochBoundary := time.UnixMilli(1).In(time.FixedZone("previous-year", -3600))
	fields["authKeyExpiresAt"] = int64(300_001)
	fields["pairExpiresAt"] = int64(180_001)
	if _, err := decodeEnrollmentPayload(enrollmentTestQR(t, fields), epochBoundary); err != nil {
		t.Fatal("UTC year boundary depended on phone timezone")
	}
	for _, now := range []time.Time{{}, time.UnixMilli(-1), time.Date(10000, 1, 1, 0, 0, 0, 0, time.UTC)} {
		if _, err := decodeEnrollmentPayload(enrollmentTestQR(t, enrollmentTestFields()), now); err == nil {
			t.Error("accepted an unusable local clock")
		}
	}
}

func TestEnrollmentPayloadProjectsOnlyDocumentFields(t *testing.T) {
	fields := enrollmentTestFields()
	payload, err := decodeEnrollmentPayload(enrollmentTestQR(t, fields), enrollmentTestNow)
	if err != nil {
		t.Fatal(err)
	}
	secret := [32]byte{1, 2, 3, 4}
	bootstrap, err := payload.documentBootstrap(42, "attempt-123", secret, enrollmentTestNow.Add(time.Second))
	if err != nil {
		t.Fatal(err)
	}
	raw, err := bootstrap.documentJSON()
	if err != nil {
		t.Fatal(err)
	}
	var projected map[string]any
	if err := json.Unmarshal(raw, &projected); err != nil {
		t.Fatal(err)
	}
	want := map[string]any{"generation": float64(42), "origin": fields["origin"], "enrollmentId": fields["enrollmentId"],
		"attemptId": "attempt-123", "attemptSecret": hex.EncodeToString(secret[:]), "pairTicket": fields["pairTicket"],
		"deadline": float64(fields["pairExpiresAt"].(int64))}
	if len(projected) != len(want) {
		t.Fatal("bootstrap contains unexpected fields")
	}
	for name, value := range want {
		if projected[name] != value {
			t.Errorf("unexpected bootstrap field %s", name)
		}
	}
	if strings.Contains(string(raw), fields["authKey"].(string)) || strings.Contains(string(raw), "authKey") {
		t.Fatal("native auth key leaked into the document")
	}
}

func TestEnrollmentPayloadRejectsInvalidBootstrap(t *testing.T) {
	payload, err := decodeEnrollmentPayload(enrollmentTestQR(t, enrollmentTestFields()), enrollmentTestNow)
	if err != nil {
		t.Fatal(err)
	}
	secret := [32]byte{1}
	for _, now := range []time.Time{enrollmentTestNow.Add(-time.Millisecond), enrollmentTestNow.Add(180 * time.Second), {}} {
		if _, err := payload.documentBootstrap(1, "attempt", secret, now); err == nil {
			t.Error("bootstrap accepted rollback, expiry or invalid clock")
		}
	}
	for _, generation := range []uint64{0, 1 << 53} {
		if _, err := payload.documentBootstrap(generation, "attempt", secret, enrollmentTestNow); err == nil {
			t.Error("bootstrap accepted absent/unsafe generation")
		}
	}
	for _, attempt := range []string{"", strings.Repeat("a", 129), "bad attempt"} {
		if _, err := payload.documentBootstrap(1, attempt, secret, enrollmentTestNow); err == nil {
			t.Error("bootstrap accepted malformed attempt ID")
		}
	}
	if _, err := payload.documentBootstrap(1, "attempt", [32]byte{}, enrollmentTestNow); err == nil {
		t.Fatal("bootstrap accepted an absent attempt secret")
	}
	if _, err := (enrollmentPayload{}).documentBootstrap(1, "attempt", secret, enrollmentTestNow); err == nil {
		t.Fatal("bootstrap accepted an undecoded payload")
	}
	if _, err := (enrollmentBootstrap{}).documentJSON(); err == nil {
		t.Fatal("serialized an uninitialized document bootstrap")
	}
}

func TestEnrollmentPayloadRedactsDefaultOutputAndErrors(t *testing.T) {
	fields := enrollmentTestFields()
	qr := enrollmentTestQR(t, fields)
	payload, err := decodeEnrollmentPayload(qr, enrollmentTestNow)
	if err != nil {
		t.Fatal(err)
	}
	secret := [32]byte{1}
	bootstrap, err := payload.documentBootstrap(1, "attempt", secret, enrollmentTestNow)
	if err != nil {
		t.Fatal(err)
	}
	outputs := []string{payload.String(), payload.GoString(), bootstrap.String(), bootstrap.GoString()}
	for _, value := range []any{payload, &payload, bootstrap, &bootstrap} {
		for _, format := range []string{"%v", "%+v", "%#v", "%s", "%q", "%x", "%d"} {
			outputs = append(outputs, fmt.Sprintf(format, value))
		}
		raw, err := json.Marshal(value)
		if err != nil {
			t.Fatal(err)
		}
		outputs = append(outputs, string(raw))
	}
	fields["unknown"] = fields["authKey"]
	_, err = decodeEnrollmentPayload(enrollmentTestQR(t, fields), enrollmentTestNow)
	if err == nil {
		t.Fatal("invalid envelope did not fail")
	}
	outputs = append(outputs, err.Error(), fmt.Sprintf("%+v", err))
	for _, output := range outputs {
		for _, sensitive := range []string{fields["authKey"].(string), fields["pairTicket"].(string), hex.EncodeToString(secret[:]), qr} {
			if strings.Contains(output, sensitive) {
				t.Fatal("default formatting, JSON or error disclosed a secret")
			}
		}
	}
}
