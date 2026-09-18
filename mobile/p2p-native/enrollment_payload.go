package main

import (
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"strconv"
	"strings"
	"time"
	"unicode/utf8"
)

const enrollmentQRPrefix = "neige-enroll:v2:"

var (
	errEnrollmentPayload = errors.New("无效或不受支持的入网二维码")
	errEnrollmentTime    = errors.New("二维码期限或手机时间异常，请重新扫码")
)

// enrollmentPayload stays inside the native enrollment owner. It must not be
// returned through JNI, logged, persisted as a complete QR, or sent to the page.
// Only nativeAuthKey intentionally releases the provider key for LocalClient.
type enrollmentPayload struct {
	enrollmentID     string
	origin           string
	authKey          string
	authKeyExpiresAt int64
	pairTicket       string
	pairExpiresAt    int64
	decodedAt        int64
}

func (enrollmentPayload) String() string               { return "enrollmentPayload{redacted}" }
func (p enrollmentPayload) GoString() string           { return p.String() }
func (p enrollmentPayload) Format(f fmt.State, _ rune) { _, _ = io.WriteString(f, p.String()) }
func (enrollmentPayload) MarshalJSON() ([]byte, error) { return []byte(`{"redacted":true}`), nil }
func (p enrollmentPayload) nativeAuthKey() string      { return p.authKey }

func enrollmentIDValid(s string) bool {
	if len(s) == 0 || len(s) > 128 {
		return false
	}
	for _, c := range s {
		if !(c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9' || c == '-' || c == '_') {
			return false
		}
	}
	return true
}

func enrollmentAuthKeyValid(s string) bool {
	// Check credential family, not the provider's opaque internal key format.
	if !strings.HasPrefix(s, "tskey-auth-") || len(s) <= len("tskey-auth-") || len(s) > 1024 {
		return false
	}
	for _, c := range s {
		if c < '!' || c > '~' {
			return false
		}
	}
	return true
}

func enrollmentTicketValid(s string) bool {
	if len(s) != 64 {
		return false
	}
	for _, c := range s {
		if !(c >= '0' && c <= '9' || c >= 'a' && c <= 'f') {
			return false
		}
	}
	return true
}

// Decode exactly one flat object without the duplicate-key overwrite or
// case-insensitive field matching of json.Unmarshal into a struct. All parser
// errors are replaced; decoder errors can quote attacker-supplied secrets.
func enrollmentFields(raw []byte) (map[string]json.Token, error) {
	decoder := json.NewDecoder(strings.NewReader(string(raw)))
	decoder.UseNumber()
	start, err := decoder.Token()
	if err != nil || start != json.Delim('{') {
		return nil, errEnrollmentPayload
	}
	fields := make(map[string]json.Token, 7)
	for decoder.More() {
		token, err := decoder.Token()
		if err != nil {
			return nil, errEnrollmentPayload
		}
		name, ok := token.(string)
		if !ok {
			return nil, errEnrollmentPayload
		}
		switch name {
		case "version", "enrollmentId", "origin", "authKey", "authKeyExpiresAt", "pairTicket", "pairExpiresAt":
		default:
			return nil, errEnrollmentPayload
		}
		if _, duplicate := fields[name]; duplicate {
			return nil, errEnrollmentPayload
		}
		value, err := decoder.Token()
		if err != nil {
			return nil, errEnrollmentPayload
		}
		switch value.(type) {
		case string, json.Number:
		default:
			return nil, errEnrollmentPayload
		}
		fields[name] = value
	}
	end, err := decoder.Token()
	if err != nil || end != json.Delim('}') || len(fields) != 7 {
		return nil, errEnrollmentPayload
	}
	if _, err := decoder.Token(); err != io.EOF {
		return nil, errEnrollmentPayload
	}
	return fields, nil
}

func enrollmentInteger(value json.Token) (int64, bool) {
	number, ok := value.(json.Number)
	if !ok {
		return 0, false
	}
	n, err := strconv.ParseInt(string(number), 10, 64)
	return n, err == nil
}

func enrollmentNow(now time.Time) (int64, bool) {
	year := now.UTC().Year()
	if year < 1970 || year > 9999 {
		return 0, false
	}
	ms := now.UnixMilli()
	return ms, ms > 0 && ms <= 253402300799999
}

func decodeEnrollmentPayload(raw string, now time.Time) (enrollmentPayload, error) {
	if len(raw) > 2048 || !strings.HasPrefix(raw, enrollmentQRPrefix) {
		return enrollmentPayload{}, errEnrollmentPayload
	}
	encoded := strings.TrimPrefix(raw, enrollmentQRPrefix)
	data, err := base64.RawURLEncoding.Strict().DecodeString(encoded)
	if err != nil || base64.RawURLEncoding.EncodeToString(data) != encoded || !utf8.Valid(data) {
		return enrollmentPayload{}, errEnrollmentPayload
	}
	fields, err := enrollmentFields(data)
	if err != nil {
		return enrollmentPayload{}, err
	}
	version, versionOK := enrollmentInteger(fields["version"])
	id, idOK := fields["enrollmentId"].(string)
	origin, originOK := fields["origin"].(string)
	key, keyOK := fields["authKey"].(string)
	ticket, ticketOK := fields["pairTicket"].(string)
	keyExpiry, keyExpiryOK := enrollmentInteger(fields["authKeyExpiresAt"])
	pairExpiry, pairExpiryOK := enrollmentInteger(fields["pairExpiresAt"])
	if !versionOK || version != 2 || !idOK || !enrollmentIDValid(id) || !originOK || len(origin) == 0 || len(origin) > 512 ||
		!keyOK || !enrollmentAuthKeyValid(key) || !ticketOK || !enrollmentTicketValid(ticket) || !keyExpiryOK || !pairExpiryOK {
		return enrollmentPayload{}, errEnrollmentPayload
	}
	if _, err := parseTailnetOrigin(origin); err != nil {
		return enrollmentPayload{}, errEnrollmentPayload
	}
	nowMS, validClock := enrollmentNow(now)
	// Only a local plausibility fence: allow 60s of phone clock skew on the
	// maximum horizon. Never extend the supplied deadlines or claim cloud key
	// lifetime/capabilities are proved. The provider/server enforce real expiry.
	if !validClock || keyExpiry <= nowMS || keyExpiry > 253402300799999 || keyExpiry-nowMS > 360_000 ||
		pairExpiry <= nowMS || pairExpiry-nowMS > 240_000 || pairExpiry > keyExpiry {
		return enrollmentPayload{}, errEnrollmentTime
	}
	return enrollmentPayload{enrollmentID: id, origin: origin, authKey: key, authKeyExpiresAt: keyExpiry,
		pairTicket: ticket, pairExpiresAt: pairExpiry, decodedAt: nowMS}, nil
}

// enrollmentBootstrap is a one-document projection, never a saved resume
// record. Default output is redacted; documentJSON is the explicit disclosure
// boundary for the ticket and attempt secret, and cannot include the auth key.
type enrollmentBootstrap struct {
	generation    uint64
	origin        string
	enrollmentID  string
	attemptID     string
	attemptSecret string
	pairTicket    string
	deadline      int64
}

func (enrollmentBootstrap) String() string               { return "enrollmentBootstrap{redacted}" }
func (b enrollmentBootstrap) GoString() string           { return b.String() }
func (b enrollmentBootstrap) Format(f fmt.State, _ rune) { _, _ = io.WriteString(f, b.String()) }
func (enrollmentBootstrap) MarshalJSON() ([]byte, error) { return []byte(`{"redacted":true}`), nil }

// The native owner may call this only after peer and TLS validation, binding
// the attempt to one APK top-level document and clearing pending disk secrets.
// This pure projection neither establishes those facts nor grants a session.
func (p enrollmentPayload) documentBootstrap(generation uint64, attemptID string, attemptSecret [32]byte, now time.Time) (enrollmentBootstrap, error) {
	nowMS, validClock := enrollmentNow(now)
	if !validClock || p.decodedAt == 0 || nowMS < p.decodedAt || nowMS >= p.pairExpiresAt {
		return enrollmentBootstrap{}, errEnrollmentTime
	}
	if generation == 0 || generation > 1<<53-1 || !enrollmentIDValid(attemptID) || attemptSecret == [32]byte{} {
		return enrollmentBootstrap{}, errEnrollmentPayload
	}
	return enrollmentBootstrap{generation: generation, origin: p.origin, enrollmentID: p.enrollmentID,
		attemptID: attemptID, attemptSecret: hex.EncodeToString(attemptSecret[:]), pairTicket: p.pairTicket, deadline: p.pairExpiresAt}, nil
}

func (b enrollmentBootstrap) documentJSON() ([]byte, error) {
	if b.generation == 0 {
		return nil, errEnrollmentPayload
	}
	return json.Marshal(struct {
		Generation    uint64 `json:"generation"`
		Origin        string `json:"origin"`
		EnrollmentID  string `json:"enrollmentId"`
		AttemptID     string `json:"attemptId"`
		AttemptSecret string `json:"attemptSecret"`
		PairTicket    string `json:"pairTicket"`
		Deadline      int64  `json:"deadline"`
	}{b.generation, b.origin, b.enrollmentID, b.attemptID, b.attemptSecret, b.pairTicket, b.deadline})
}
