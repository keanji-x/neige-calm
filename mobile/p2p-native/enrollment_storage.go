package main

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"io"
	"os"
	"path/filepath"
	"regexp"
	"sync"
	"syscall"
)

const pendingFilename = "pending-enrollment.json"
const resetFilename = "enrollment-reset.json"

var privateRecordMu sync.Mutex
var ownedTemporary = regexp.MustCompile(`^\.enrollment-(?:(?:pending|record)-)?[0-9]{1,10}$`)

func clearInterruptedWritesLocked(dir string) error {
	info, err := os.Lstat(dir)
	if err != nil {
		return errors.New("无法确认连接记录目录")
	}
	stat, ok := info.Sys().(*syscall.Stat_t)
	if !ok || !info.IsDir() || info.Mode().Perm() != 0700 || stat.Uid != uint32(os.Getuid()) {
		return errors.New("连接记录目录归属或权限异常")
	}
	entries, err := os.ReadDir(dir)
	if err != nil {
		return errors.New("无法确认临时入网凭证已清除")
	}
	var owned []string
	for _, entry := range entries {
		if !ownedTemporary.MatchString(entry.Name()) {
			continue
		}
		path := filepath.Join(dir, entry.Name())
		info, err := os.Lstat(path)
		if err != nil {
			return errors.New("无法确认临时入网凭证已清除")
		}
		stat, ok := info.Sys().(*syscall.Stat_t)
		if !ok || stat.Uid != uint32(os.Getuid()) || !info.Mode().IsRegular() || info.Mode().Perm() != 0600 || info.Size() > 8192 {
			return errors.New("临时连接记录归属或权限异常，无法安全清理")
		}
		owned = append(owned, path)
	}
	for _, path := range owned {
		if err := os.Remove(path); err != nil {
			return errors.New("无法清除临时入网凭证")
		}
	}
	// Sync even after a previous removal whose sync may have failed.
	return syncPrivateDirectory(dir)
}

func syncPrivateDirectory(dir string) error {
	directory, err := os.Open(dir)
	if err == nil {
		err = directory.Sync()
		directory.Close()
	}
	if err != nil {
		return errors.New("无法确认连接记录已持久保存")
	}
	return nil
}

func requireNoPendingReset(dir string) error {
	if _, err := os.Lstat(filepath.Join(dir, resetFilename)); !os.IsNotExist(err) {
		return errors.New("上次退出入网尚未确认，请在连接页重新确认退出后再扫码")
	}
	return nil
}

// Private node directory is supplied by Android noBackupFilesDir. Reject
// symlinks/non-private files and never include their contents in an error.
func readPrivateJSON(dir, name string, into any) error {
	path := filepath.Join(dir, name)
	info, err := os.Lstat(path)
	if err != nil {
		return err
	}
	if !info.Mode().IsRegular() || info.Mode().Perm() != 0600 || info.Size() > 8192 {
		return errors.New("本地连接记录无效")
	}
	file, err := os.Open(path)
	if err != nil {
		return errors.New("无法读取本地连接记录")
	}
	defer file.Close()
	decoder := json.NewDecoder(io.LimitReader(file, 8193))
	decoder.DisallowUnknownFields()
	if decoder.Decode(into) != nil {
		return errors.New("本地连接记录无效")
	}
	var extra any
	if decoder.Decode(&extra) != io.EOF {
		return errors.New("本地连接记录无效")
	}
	return nil
}
func writePrivateJSON(dir, name string, value any) error {
	privateRecordMu.Lock()
	defer privateRecordMu.Unlock()
	if err := clearInterruptedWritesLocked(dir); err != nil {
		return err
	}
	data, err := json.Marshal(value)
	if err != nil || len(data) > 8192 {
		return errors.New("无法保存本地连接记录")
	}
	pattern := ".enrollment-record-*"
	if name == pendingFilename {
		pattern = ".enrollment-pending-*"
	}
	file, err := os.CreateTemp(dir, pattern)
	if err != nil {
		return errors.New("无法保存本地连接记录")
	}
	defer os.Remove(file.Name())
	if err = file.Chmod(0600); err == nil {
		_, err = file.Write(data)
	}
	if err == nil {
		err = file.Sync()
	}
	closeErr := file.Close()
	if err == nil {
		err = closeErr
	}
	if err == nil {
		err = os.Rename(file.Name(), filepath.Join(dir, name))
	}
	if err == nil {
		var directory *os.File
		directory, err = os.Open(dir)
		if err == nil {
			err = directory.Sync()
			directory.Close()
		}
	}
	if err != nil {
		return errors.New("无法保存本地连接记录")
	}
	return nil
}
func clearPending(dir string) error {
	privateRecordMu.Lock()
	defer privateRecordMu.Unlock()
	if err := clearInterruptedWritesLocked(dir); err != nil {
		return err
	}
	return removePrivateRecordLocked(dir, pendingFilename)
}
func removePrivateRecord(dir, name string) error {
	privateRecordMu.Lock()
	defer privateRecordMu.Unlock()
	return removePrivateRecordLocked(dir, name)
}
func removePrivateRecordLocked(dir, name string) error {
	err := os.Remove(filepath.Join(dir, name))
	if os.IsNotExist(err) {
		return syncPrivateDirectory(dir)
	}
	if err != nil {
		return errors.New("无法清除入网凭证，请检查设备存储")
	}
	directory, err := os.Open(dir)
	if err == nil {
		err = directory.Sync()
		directory.Close()
	}
	if err != nil {
		return errors.New("无法清除入网凭证，请检查设备存储")
	}
	return nil
}

type pendingEnrollment struct {
	EnrollmentID     string `json:"enrollmentId"`
	Origin           string `json:"origin"`
	Stage            string `json:"stage"`
	DecodedAt        int64  `json:"decodedAt"`
	AuthKey          string `json:"authKey,omitempty"`
	AuthKeyExpiresAt int64  `json:"authKeyExpiresAt"`
	PairTicket       string `json:"pairTicket"`
	PairExpiresAt    int64  `json:"pairExpiresAt"`
	AttemptID        string `json:"attemptId"`
	AttemptSecret    string `json:"attemptSecret"`
}
type savedTargets struct {
	Version  int              `json:"version"`
	Bindings []tailnetBinding `json:"bindings"`
}

func loadTargets(dir string) (savedTargets, error) {
	var saved savedTargets
	err := readPrivateJSON(dir, "tailnet-targets.json", &saved)
	if os.IsNotExist(err) {
		return savedTargets{Version: 1, Bindings: []tailnetBinding{}}, nil
	}
	if err != nil {
		return savedTargets{}, err
	}
	if saved.Version != 1 || len(saved.Bindings) > 8 {
		return savedTargets{}, errors.New("连接目标记录无效")
	}
	seen := make(map[string]bool)
	for _, binding := range saved.Bindings {
		if _, err := parseTailnetOrigin(binding.Origin); err != nil || binding.SchemaVersion != 1 || binding.PeerID.IsZero() || !targetNodeAddress(binding.TailnetIP) || seen[binding.Origin] {
			return savedTargets{}, errors.New("连接目标记录无效")
		}
		seen[binding.Origin] = true
	}
	return saved, nil
}
func saveTarget(dir string, binding tailnetBinding) error {
	saved, err := loadTargets(dir)
	if err != nil {
		return err
	}
	for index, previous := range saved.Bindings {
		if previous.Origin == binding.Origin {
			saved.Bindings[index] = binding
			return writePrivateJSON(dir, "tailnet-targets.json", saved)
		}
	}
	if len(saved.Bindings) == 8 {
		return errors.New("已保存的工作区达到上限，请先移除旧配置")
	}
	saved.Bindings = append(saved.Bindings, binding)
	return writePrivateJSON(dir, "tailnet-targets.json", saved)
}
func savedTarget(dir, origin string) (tailnetBinding, error) {
	saved, err := loadTargets(dir)
	if err != nil {
		return tailnetBinding{}, err
	}
	for _, binding := range saved.Bindings {
		if binding.Origin == origin {
			return binding, nil
		}
	}
	return tailnetBinding{}, errors.New("请先扫描这个工作区的添加手机二维码")
}

// A failed registration may have reached the control plane. Retrying exactly
// that invitation/key is safe; a different key must not replace an unknown
// identity. Digests retain no bearer credential across process death.
type registrationRecord struct {
	Version          int    `json:"version"`
	EnrollmentID     string `json:"enrollmentId"`
	Origin           string `json:"origin"`
	KeyDigest        string `json:"keyDigest"`
	TicketDigest     string `json:"ticketDigest"`
	AuthKeyExpiresAt int64  `json:"authKeyExpiresAt"`
	PairExpiresAt    int64  `json:"pairExpiresAt"`
}

func registrationFor(payload enrollmentPayload) registrationRecord {
	key := sha256.Sum256([]byte(payload.authKey))
	ticket := sha256.Sum256([]byte(payload.pairTicket))
	return registrationRecord{1, payload.enrollmentID, payload.origin, hex.EncodeToString(key[:]), hex.EncodeToString(ticket[:]), payload.authKeyExpiresAt, payload.pairExpiresAt}
}
func registrationAttempted(dir string, payload enrollmentPayload) (attempted, same bool, err error) {
	var record registrationRecord
	err = readPrivateJSON(dir, "registration-attempt.json", &record)
	if os.IsNotExist(err) {
		return false, false, nil
	}
	if err != nil || record.Version != 1 || !enrollmentIDValid(record.EnrollmentID) || !enrollmentTicketValid(record.KeyDigest) || !enrollmentTicketValid(record.TicketDigest) {
		return true, false, errors.New("无法确认先前入网结果，请检查手机连接状态")
	}
	return true, record == registrationFor(payload), nil
}
