package main

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"strconv"
	"time"
)

func parsedPort(origin tailnetOrigin) string { return strconv.Itoa(int(origin.port)) }

// Cancellation is an engine-owned epoch and context, independent of Java's
// executor interruption. It closes every socket of the retired document.
func (e *engine) cancelEnrollment() error {
	e.enrollmentMu.Lock()
	defer e.enrollmentMu.Unlock()
	e.enrollmentGeneration++
	if e.enrollmentCancel != nil {
		e.enrollmentCancel()
		e.enrollmentCancel = nil
	}
	e.closeTunnelLocked()
	return clearPending(e.dir)
}

func (e *engine) enroll(raw string, permit *operationPermit) (result json.RawMessage, resultErr error) {
	payload, err := decodeEnrollmentPayload(raw, time.Now())
	if err != nil {
		return nil, err
	}
	// Publish a new generation only after invalidating all old native I/O.
	permit.mu.Lock()
	if permit.ctx.Err() != nil {
		permit.mu.Unlock()
		return nil, errors.New("原生操作已取消")
	}
	e.enrollmentMu.Lock()
	if e.resetting {
		e.enrollmentMu.Unlock()
		permit.mu.Unlock()
		return nil, errors.New("正在重置手机入网，请稍后扫码")
	}
	if err := requireNoPendingReset(e.dir); err != nil {
		e.enrollmentMu.Unlock()
		permit.mu.Unlock()
		return nil, err
	}
	e.enrollmentGeneration++
	generation := e.enrollmentGeneration
	if e.enrollmentCancel != nil {
		e.enrollmentCancel()
	}
	e.closeTunnelLocked()
	ctx, cancel := context.WithDeadline(permit.ctx, time.UnixMilli(payload.pairExpiresAt))
	e.enrollmentCancel = cancel
	permit.stop = func() error { return e.cancelGeneration(generation) }
	var id, secret [32]byte
	_, idErr := rand.Read(id[:])
	_, secretErr := rand.Read(secret[:])
	if idErr != nil || secretErr != nil {
		e.enrollmentMu.Unlock()
		permit.mu.Unlock()
		cancel()
		return nil, errors.New("无法创建扫码操作")
	}
	attemptID := hex.EncodeToString(id[:])
	pending := pendingEnrollment{payload.enrollmentID, payload.origin, "joining", payload.decodedAt, payload.nativeAuthKey(), payload.authKeyExpiresAt, payload.pairTicket, payload.pairExpiresAt, attemptID, hex.EncodeToString(secret[:])}
	err = writePrivateJSON(e.dir, pendingFilename, pending)
	e.enrollmentMu.Unlock()
	permit.mu.Unlock()
	if err != nil {
		cancel()
		return nil, err
	}
	defer cancel()
	defer func() {
		e.enrollmentMu.Lock()
		defer e.enrollmentMu.Unlock()
		if e.enrollmentGeneration == generation {
			e.enrollmentCancel = nil
			if cleanupErr := clearPending(e.dir); cleanupErr != nil {
				result = nil
				resultErr = cleanupErr
			}
		}
	}()
	active := func() error {
		if ctx.Err() != nil {
			return errors.New("扫码已取消或过期，请重新扫码")
		}
		return nil
	}
	var runtime tailnetRuntime
	for {
		if err = active(); err != nil {
			return nil, err
		}
		runtime, err = e.runtime()
		if err == nil {
			break
		}
		select {
		case <-ctx.Done():
		case <-time.After(100 * time.Millisecond):
		}
	}
	status, err := runtime.Status(ctx)
	if err != nil {
		return nil, errors.New("无法读取入网状态，请重试")
	}
	if status.BackendState != "Running" {
		// Old expired or unknown identities are not silently replaced. No Logout
		// path exists here; a scan can authorize only a fresh node or this flight.
		attempted, sameAttempt, identityErr := registrationAttempted(e.dir, payload)
		if identityErr != nil {
			return nil, identityErr
		}
		hasIdentity, identityErr := runtime.HasIdentity(ctx)
		if identityErr != nil {
			return nil, identityErr
		}
		if hasIdentity || (attempted && !sameAttempt) || (e.identityClaimed.Load() && !sameAttempt) || status.CurrentTailnet != nil || (status.Self != nil && !status.Self.ID.IsZero()) {
			return nil, errors.New("手机已有或未确认的入网身份。请重新扫描同一有效二维码，或在连接页选择“重新开始手机入网”。")
		}
		if status.BackendState != "NeedsLogin" {
			return nil, errors.New("节点状态尚未就绪，请稍后重新扫码")
		}
		if time.Now().UnixMilli() >= payload.authKeyExpiresAt {
			return nil, errEnrollmentTime
		}
		if err = active(); err != nil {
			return nil, err
		}
		e.enrollmentMu.Lock()
		if generation != e.enrollmentGeneration || ctx.Err() != nil {
			e.enrollmentMu.Unlock()
			return nil, errors.New("旧扫码操作已取消")
		}
		err = writePrivateJSON(e.dir, "registration-attempt.json", registrationFor(payload))
		if err == nil {
			e.identityClaimed.Store(true)
		}
		e.enrollmentMu.Unlock()
		if err != nil {
			return nil, err
		}
		if err = runtime.StartAuth(ctx, payload.nativeAuthKey()); err != nil {
			return nil, errors.New("设备入网未完成，请检查网络或重新扫码")
		}
		for status.BackendState != "Running" {
			select {
			case <-ctx.Done():
				return nil, errors.New("设备入网超时；已创建的身份会保留，请重新扫码")
			case <-time.After(200 * time.Millisecond):
			}
			status, err = runtime.Status(ctx)
			if err != nil {
				return nil, errors.New("无法确认设备入网状态，请重新扫码")
			}
			if status.BackendState == "NeedsMachineAuth" {
				return nil, errors.New("设备等待网络管理员批准；手机不会打开登录页面")
			}
		}
	}
	if err = active(); err != nil {
		return nil, err
	}
	e.enrollmentMu.Lock()
	if generation != e.enrollmentGeneration {
		e.enrollmentMu.Unlock()
		return nil, errors.New("旧扫码操作已取消")
	}
	if err = removePrivateRecord(e.dir, "registration-attempt.json"); err != nil {
		e.enrollmentMu.Unlock()
		return nil, err
	}
	pending.AuthKey = ""
	pending.Stage = "target"
	err = writePrivateJSON(e.dir, pendingFilename, pending)
	e.enrollmentMu.Unlock()
	payload.authKey = ""
	if err != nil {
		return nil, err
	}
	binding, err := resolveTailnetTarget(payload.origin, status)
	if err != nil {
		return nil, err
	}
	tlsCtx, tlsCancel := context.WithTimeout(ctx, 8*time.Second)
	err = e.verifyTargetTLS(tlsCtx, binding)
	tlsCancel()
	if err != nil {
		return nil, err
	}
	permit.mu.Lock()
	defer permit.mu.Unlock()
	e.enrollmentMu.Lock()
	defer e.enrollmentMu.Unlock()
	if err = active(); err != nil || generation != e.enrollmentGeneration {
		return nil, errors.New("旧扫码操作已取消")
	}
	bootstrap, err := payload.documentBootstrap(generation, attemptID, secret, time.Now())
	if err != nil {
		return nil, err
	}
	// Disk cleanup precedes the only handoff. A failed cleanup never discloses
	// the bootstrap and a process restart cannot reconstruct it.
	if err = clearPending(e.dir); err != nil {
		return nil, err
	}
	if err = saveTarget(e.dir, binding); err != nil {
		return nil, err
	}
	tunnel, err := e.installTunnelLocked(binding)
	if err != nil {
		return nil, err
	}
	data, err := bootstrap.documentJSON()
	if err != nil {
		tunnel.close()
		return nil, err
	}
	return json.Marshal(struct {
		OK        bool            `json:"ok"`
		Origin    string          `json:"origin"`
		Proxy     string          `json:"proxy"`
		Bootstrap json.RawMessage `json:"bootstrap"`
	}{true, binding.Origin, tunnel.proxyURL(), data})
}

func (e *engine) bindTailnet(origin string) (string, error) {
	e.enrollmentMu.Lock()
	defer e.enrollmentMu.Unlock()
	if err := requireNoPendingReset(e.dir); err != nil {
		return "", err
	}
	binding, err := savedTarget(e.dir, origin)
	if err != nil {
		return "", err
	}
	if e.tunnel != nil && e.tunnel.binding == binding && e.tunnel.ctx.Err() == nil {
		return e.tunnel.proxyURL(), nil
	}
	tunnel, err := e.installTunnelLocked(binding)
	if err != nil {
		return "", err
	}
	return tunnel.proxyURL(), nil
}

// Called only after the packaged launcher obtains explicit native confirmation.
// SDK Logout supplies the authority boundary; never remove or replace its state.
func (e *engine) resetEnrollment(permit *operationPermit) error {
	permit.mu.Lock()
	if permit.ctx.Err() != nil {
		permit.mu.Unlock()
		return errors.New("原生操作已取消")
	}
	e.enrollmentMu.Lock()
	if e.resetting {
		e.enrollmentMu.Unlock()
		permit.mu.Unlock()
		return errors.New("正在重置手机入网")
	}
	e.enrollmentGeneration++
	if e.enrollmentCancel != nil {
		e.enrollmentCancel()
		e.enrollmentCancel = nil
	}
	e.closeTunnelLocked()
	// A lost Logout reply must not become implicit consent to join again on
	// restart. Clear this secret-free fence only after confirmed SDK success.
	if err := writePrivateJSON(e.dir, resetFilename, struct {
		Version int `json:"version"`
	}{1}); err != nil {
		e.enrollmentMu.Unlock()
		permit.mu.Unlock()
		return err
	}
	e.resetting = true
	generation := e.enrollmentGeneration
	ctx, cancel := context.WithTimeout(permit.ctx, 8*time.Second)
	e.enrollmentCancel = cancel
	permit.stop = func() error { return e.cancelGeneration(generation) }
	e.enrollmentMu.Unlock()
	permit.mu.Unlock()
	defer func() {
		cancel()
		e.enrollmentMu.Lock()
		e.resetting = false
		if generation == e.enrollmentGeneration {
			e.enrollmentCancel = nil
		}
		e.enrollmentMu.Unlock()
	}()
	runtime, err := e.runtime()
	if err != nil {
		return err
	}
	if err = runtime.Logout(ctx); err != nil {
		return errors.New("退出手机入网未完成，请联网后重试；已保存身份未被删除")
	}
	for {
		known, checkErr := runtime.HasIdentity(ctx)
		status, statusErr := runtime.Status(ctx)
		if checkErr == nil && statusErr == nil && !known && status.BackendState != "Running" && status.CurrentTailnet == nil && (status.Self == nil || status.Self.ID.IsZero()) {
			break
		}
		select {
		case <-ctx.Done():
			return errors.New("无法确认手机已退出网络，请重试")
		case <-time.After(100 * time.Millisecond):
		}
	}
	permit.mu.Lock()
	defer permit.mu.Unlock()
	e.enrollmentMu.Lock()
	defer e.enrollmentMu.Unlock()
	if generation != e.enrollmentGeneration || ctx.Err() != nil {
		return errors.New("本次重置已取消，请重新检查连接状态")
	}
	if err := removePrivateRecord(e.dir, "registration-attempt.json"); err != nil {
		return err
	}
	if err := clearPending(e.dir); err != nil {
		return err
	}
	if err := removePrivateRecord(e.dir, "tailnet-targets.json"); err != nil {
		return err
	}
	if err := removePrivateRecord(e.dir, resetFilename); err != nil {
		return err
	}
	e.identityClaimed.Store(false)
	return nil
}

func (e *engine) cancelGeneration(generation uint64) error {
	e.enrollmentMu.Lock()
	defer e.enrollmentMu.Unlock()
	if e.enrollmentGeneration != generation {
		return nil
	}
	if e.enrollmentCancel != nil {
		e.enrollmentCancel()
		e.enrollmentCancel = nil
	}
	e.closeTunnelLocked()
	return clearPending(e.dir)
}
