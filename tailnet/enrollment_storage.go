package main

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"io"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"golang.org/x/sys/unix"
)

type enrollmentConfig struct {
	SchemaVersion int      `json:"schemaVersion"`
	ClientID      string   `json:"clientId"`
	SecretFile    string   `json:"secretFile"`
	PhoneTags     []string `json:"phoneTags"`
	Tailnet       string   `json:"expectedTailnet"`
	Origin        string   `json:"expectedOrigin"`
}

// Walk with directory handles: neither intermediate nor final symlinks are
// followed, including during a concurrent path replacement.
func privateDirectory(path string) (*os.File, error) {
	if !filepath.IsAbs(path) || filepath.Clean(path) != path {
		return nil, errors.New("private absolute path required")
	}
	fd, err := unix.Open("/", unix.O_RDONLY|unix.O_DIRECTORY|unix.O_CLOEXEC, 0)
	if err != nil {
		return nil, err
	}
	for _, part := range strings.Split(strings.TrimPrefix(path, "/"), "/") {
		if part == "" {
			continue
		}
		next, e := unix.Openat(fd, part, unix.O_RDONLY|unix.O_DIRECTORY|unix.O_NOFOLLOW|unix.O_CLOEXEC, 0)
		unix.Close(fd)
		if e != nil {
			return nil, e
		}
		fd = next
	}
	var st unix.Stat_t
	if err = unix.Fstat(fd, &st); err != nil || st.Uid != uint32(os.Geteuid()) || st.Mode&07777 != 0700 {
		unix.Close(fd)
		return nil, errors.New("directory must belong to this user with mode 0700")
	}
	return os.NewFile(uintptr(fd), path), nil
}

func privateReadAt(dir *os.File, name string, limit int64) ([]byte, error) {
	fd, err := unix.Openat(int(dir.Fd()), name, unix.O_RDONLY|unix.O_NOFOLLOW|unix.O_CLOEXEC|unix.O_NONBLOCK, 0)
	if err != nil {
		return nil, err
	}
	f := os.NewFile(uintptr(fd), name)
	defer f.Close()
	var st unix.Stat_t
	if unix.Fstat(fd, &st) != nil || st.Uid != uint32(os.Geteuid()) || st.Mode&unix.S_IFMT != unix.S_IFREG || st.Mode&07777 != 0600 || st.Nlink != 1 {
		return nil, errors.New("file must be a private regular file owned by this user")
	}
	data, err := io.ReadAll(io.LimitReader(f, limit+1))
	if err != nil || int64(len(data)) > limit {
		return nil, errors.New("private file unavailable or oversized")
	}
	return data, nil
}

func privateRead(path string, limit int64) ([]byte, error) {
	if !filepath.IsAbs(path) || filepath.Clean(path) != path {
		return nil, errors.New("invalid private path")
	}
	dir, err := privateDirectory(filepath.Dir(path))
	if err != nil {
		return nil, err
	}
	defer dir.Close()
	return privateReadAt(dir, filepath.Base(path), limit)
}

// encoding/json normally accepts duplicate keys. Configuration and control
// messages must reject them before typed decoding.
func strictJSON(data []byte, value any) error {
	d := json.NewDecoder(bytes.NewReader(data))
	var walk func() error
	walk = func() error {
		t, err := d.Token()
		if err != nil {
			return err
		}
		if delim, ok := t.(json.Delim); ok {
			switch delim {
			case '{':
				seen := map[string]bool{}
				for d.More() {
					key, e := d.Token()
					if e != nil {
						return e
					}
					name, ok := key.(string)
					if !ok || seen[strings.ToLower(name)] {
						return errors.New("duplicate JSON key")
					}
					seen[strings.ToLower(name)] = true
					if e = walk(); e != nil {
						return e
					}
				}
			case '[':
				for d.More() {
					if e := walk(); e != nil {
						return e
					}
				}
			default:
				return errors.New("unexpected JSON delimiter")
			}
			_, err = d.Token()
		}
		return err
	}
	if err := walk(); err != nil {
		return err
	}
	if _, err := d.Token(); err != io.EOF {
		return errors.New("trailing JSON")
	}
	d = json.NewDecoder(bytes.NewReader(data))
	d.DisallowUnknownFields()
	return d.Decode(value)
}

func loadEnrollmentConfig(path string) (enrollmentConfig, string, error) {
	var c enrollmentConfig
	data, err := privateRead(path, 16384)
	if err != nil {
		return c, "", err
	}
	if !exactFields(data, "schemaVersion", "clientId", "secretFile", "phoneTags", "expectedTailnet", "expectedOrigin") {
		return c, "", errors.New("invalid enrollment configuration fields")
	}
	if err = strictJSON(data, &c); err != nil {
		return c, "", errors.New("invalid enrollment configuration")
	}
	if c.SchemaVersion != 1 || !safeID(c.ClientID) || !filepath.IsAbs(c.SecretFile) || c.Tailnet == "" || c.Tailnet == "-" || len(c.Tailnet) > 253 || strings.ContainsAny(c.Tailnet, "/\\\r\n\x00") || !strings.HasPrefix(c.Origin, "https://") || !validDNSName(strings.TrimPrefix(c.Origin, "https://")) || len(c.PhoneTags) == 0 || len(c.PhoneTags) > 8 {
		return c, "", errors.New("invalid enrollment configuration")
	}
	sort.Strings(c.PhoneTags)
	for i, tag := range c.PhoneTags {
		if !strings.HasPrefix(tag, "tag:") || !safeID(strings.TrimPrefix(tag, "tag:")) || i > 0 && tag == c.PhoneTags[i-1] {
			return c, "", errors.New("invalid phone tags")
		}
	}
	canonical, _ := json.Marshal(c)
	hash := sha256.Sum256(canonical)
	return c, hex.EncodeToString(hash[:]), nil
}

func exactFields(raw []byte, keys ...string) bool {
	var fields map[string]json.RawMessage
	if strictJSON(raw, &fields) != nil || len(fields) != len(keys) {
		return false
	}
	for _, key := range keys {
		if fields[key] == nil || bytes.Equal(fields[key], []byte("null")) {
			return false
		}
	}
	return true
}

func safeID(s string) bool {
	if len(s) == 0 || len(s) > 128 {
		return false
	}
	for _, c := range s {
		if !(c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9' || c == '_' || c == '-') {
			return false
		}
	}
	return true
}

type cleanupRecord struct {
	EnrollmentID string `json:"enrollmentId"`
	BindingHash  string `json:"bindingHash"`
	KeyID        string `json:"keyId"`
	Expires      string `json:"expires"`
	State        string `json:"state"`
	Deadline     int64  `json:"deadline"`
}
type cleanupLedger struct {
	SchemaVersion int             `json:"schemaVersion"`
	Records       []cleanupRecord `json:"records"`
}

func readLedger(dir *os.File) (cleanupLedger, error) {
	l := cleanupLedger{SchemaVersion: 1, Records: []cleanupRecord{}}
	b, err := privateReadAt(dir, "enrollment-ledger.json", 65536)
	if errors.Is(err, unix.ENOENT) {
		return l, nil
	}
	if err != nil {
		return l, err
	}
	var shape map[string]json.RawMessage
	if strictJSON(b, &shape) != nil || len(shape) != 2 || shape["schemaVersion"] == nil || shape["records"] == nil || bytes.Equal(shape["records"], []byte("null")) {
		return l, errors.New("invalid cleanup ledger fields")
	}
	var rows []map[string]json.RawMessage
	if json.Unmarshal(shape["records"], &rows) != nil {
		return l, errors.New("invalid cleanup records")
	}
	for _, row := range rows {
		if len(row) != 6 {
			return l, errors.New("invalid cleanup record fields")
		}
		for _, key := range []string{"enrollmentId", "bindingHash", "keyId", "expires", "state", "deadline"} {
			if row[key] == nil || bytes.Equal(row[key], []byte("null")) {
				return l, errors.New("missing cleanup record field")
			}
		}
	}
	if err = strictJSON(b, &l); err != nil || l.SchemaVersion != 1 || len(l.Records) > 64 {
		return l, errors.New("invalid cleanup ledger")
	}
	for _, r := range l.Records {
		if !safeID(r.EnrollmentID) || len(r.BindingHash) != 64 || len(r.Expires) > 128 || r.Deadline <= 0 || (r.State != "unknown" && r.State != "active" && r.State != "cleanup") || r.State != "unknown" && !safeID(r.KeyID) {
			return l, errors.New("invalid cleanup record")
		}
	}
	return l, nil
}

func writeLedger(dir *os.File, l cleanupLedger) error {
	data, err := json.Marshal(l)
	if err != nil {
		return err
	}
	name := "enrollment-ledger.next"
	fd, err := unix.Openat(int(dir.Fd()), name, unix.O_WRONLY|unix.O_CREAT|unix.O_EXCL|unix.O_NOFOLLOW|unix.O_CLOEXEC, 0600)
	if err != nil {
		return err
	}
	f := os.NewFile(uintptr(fd), name)
	defer f.Close()
	defer unix.Unlinkat(int(dir.Fd()), name, 0)
	if _, err = f.Write(data); err != nil {
		return err
	}
	if err = f.Sync(); err != nil {
		return err
	}
	if err = unix.Renameat(int(dir.Fd()), name, int(dir.Fd()), "enrollment-ledger.json"); err != nil {
		return err
	}
	return dir.Sync()
}
