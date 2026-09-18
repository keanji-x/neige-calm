package main

import (
	"context"
	"errors"
	"net/netip"
	"os"
	"path/filepath"
	"time"
)

// The exact origin and numeric peer authorized by the shipped v1 build. This
// migration is explicit launcher confirmation, never a general resolver on
// recovery and never permission to replace a saved stable peer binding.
const legacyTailnetOrigin = "https://pivot-neige.tail328551.ts.net:10000"

func (e *engine) confirmLegacyTarget(origin string, permit *operationPermit) error {
	if origin != legacyTailnetOrigin {
		return errors.New("旧版确认只适用于原工作区，请扫描新版二维码")
	}
	permit.mu.Lock()
	if permit.ctx.Err() != nil {
		permit.mu.Unlock()
		return errors.New("原生操作已取消")
	}
	e.enrollmentMu.Lock()
	if err := requireNoPendingReset(e.dir); err != nil {
		e.enrollmentMu.Unlock()
		permit.mu.Unlock()
		return err
	}
	e.enrollmentGeneration++
	generation := e.enrollmentGeneration
	if e.enrollmentCancel != nil {
		e.enrollmentCancel()
	}
	e.closeTunnelLocked()
	ctx, cancel := context.WithTimeout(permit.ctx, 8*time.Second)
	e.enrollmentCancel = cancel
	permit.stop = func() error { return e.cancelGeneration(generation) }
	e.enrollmentMu.Unlock()
	permit.mu.Unlock()
	defer cancel()
	runtime, err := e.runtime()
	if err != nil {
		return err
	}
	status, err := runtime.Status(ctx)
	if err != nil {
		return err
	}
	_, statErr := os.Lstat(filepath.Join(e.dir, "tailnet-targets.json"))
	migrate := os.IsNotExist(statErr)
	var binding tailnetBinding
	if migrate {
		binding, err = resolveTailnetTarget(origin, status)
		if err == nil && binding.TailnetIP != netip.MustParseAddr("100.123.126.35") {
			err = errors.New("旧工作区节点地址已改变，请扫描新版二维码")
		}
	} else {
		binding, err = savedTarget(e.dir, origin)
		if err == nil {
			_, err = validateTailnetTarget(binding, status)
		}
	}
	if err != nil {
		return err
	}
	if err := e.verifyTargetTLS(ctx, binding); err != nil {
		return err
	}
	status, err = runtime.Status(ctx)
	if err != nil {
		return err
	}
	if _, err := validateTailnetTarget(binding, status); err != nil {
		return err
	}
	permit.mu.Lock()
	defer permit.mu.Unlock()
	e.enrollmentMu.Lock()
	defer e.enrollmentMu.Unlock()
	if ctx.Err() != nil || e.enrollmentGeneration != generation {
		return errors.New("旧工作区确认已取消")
	}
	if migrate {
		return saveTarget(e.dir, binding)
	}
	return nil
}
