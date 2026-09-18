package main

/*
#include <stdlib.h>
*/
import "C"

import (
	"context"
	"crypto/rand"
	"crypto/x509"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sync"
	"sync/atomic"
	"time"

	"tailscale.com/tsnet"
)

var instance struct {
	sync.Mutex
	engine *engine
}

type engine struct {
	mu                   sync.Mutex
	dir                  string
	hostname             string
	starting             bool
	ready                bool
	startErr             error
	node                 *tsnet.Server
	started              time.Time
	transport            tailnetRuntime
	identityClaimed      atomic.Bool
	tlsRoots             *x509.CertPool
	enrollmentMu         sync.Mutex
	enrollmentGeneration uint64
	resetting            bool
	enrollmentCancel     context.CancelFunc
	tunnel               *tailnetTunnel
}

func encoded(value any) string { b, _ := json.Marshal(value); return string(b) }
func failure(err error) string { return encoded(map[string]any{"ok": false, "error": err.Error()}) }
func current() (*engine, error) {
	instance.Lock()
	defer instance.Unlock()
	if instance.engine == nil {
		return nil, fmt.Errorf("连接尚未启动")
	}
	return instance.engine, nil
}

func start(dir string) string {
	instance.Lock()
	defer instance.Unlock()
	if instance.engine != nil {
		instance.engine.startNode()
		return encoded(map[string]any{"ok": true})
	}
	if !filepath.IsAbs(dir) {
		return failure(fmt.Errorf("需要应用私有目录"))
	}
	if err := os.MkdirAll(dir, 0700); err != nil {
		return failure(err)
	}
	// tsnet accepts an explicit state directory, but its Android socket logger
	// still calls logpolicy.LogsDir without that option. Bridge our explicit,
	// app-private directory to the upstream setting before creating the node.
	// Never use /tmp, the process working directory, or change HOME on Android.
	logsDir := filepath.Join(dir, "logs")
	if err := os.MkdirAll(logsDir, 0700); err != nil {
		return failure(err)
	}
	if err := os.Setenv("TS_LOGS_DIR", logsDir); err != nil {
		return failure(err)
	}
	namePath := filepath.Join(dir, "trial-hostname")
	name, err := os.ReadFile(namePath)
	if os.IsNotExist(err) {
		var suffix [4]byte
		if _, err = rand.Read(suffix[:]); err != nil {
			return failure(err)
		}
		name = []byte("neige-p2p-" + hex.EncodeToString(suffix[:]))
		err = os.WriteFile(namePath, name, 0600)
	}
	if err != nil {
		return failure(err)
	}
	// Reopening never reconstructs a scan grant from disk. A pending record
	// contains secrets only until native hands it to its unique document.
	if err := clearPending(dir); err != nil {
		return failure(err)
	}
	e := &engine{dir: dir, hostname: string(name), started: time.Now()}
	instance.engine = e
	e.startNode()
	return encoded(map[string]any{"ok": true})
}

// A failed tsnet.Start must release the partially created writer before retry.
// This flight is independent of JNI status/proxy calls and can never block the
// Java command worker or installation of the exact-origin loopback fence.
func (e *engine) startNode() {
	e.mu.Lock()
	if e.ready || e.starting {
		e.mu.Unlock()
		return
	}
	e.starting = true
	e.startErr = nil
	node := privateTailnetNode(e.dir, e.hostname)
	e.mu.Unlock()
	go func() {
		err := node.Start()
		if err != nil {
			node.Close()
		}
		e.mu.Lock()
		defer e.mu.Unlock()
		e.starting = false
		e.startErr = err
		if err == nil {
			client, clientErr := node.LocalClient()
			if clientErr != nil {
				e.startErr = clientErr
				node.Close()
				return
			}
			e.node = node
			e.transport = &realTailnetRuntime{node: node, local: client}
			e.ready = true
			go e.watchTargets(client)
		}
	}()
}
func (e *engine) readyNode() (*tsnet.Server, error) {
	e.mu.Lock()
	defer e.mu.Unlock()
	if e.startErr != nil {
		return nil, e.startErr
	}
	if !e.ready || e.node == nil {
		return nil, fmt.Errorf("连接正在恢复")
	}
	return e.node, nil
}

func status() string {
	e, err := current()
	if err != nil {
		return failure(err)
	}
	node, err := e.readyNode()
	if err != nil {
		e.mu.Lock()
		starting := e.starting
		e.mu.Unlock()
		if starting {
			return encoded(map[string]any{"ok": true, "state": "Starting"})
		}
		return failure(err)
	}
	lc, err := node.LocalClient()
	if err != nil {
		return failure(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 4*time.Second)
	defer cancel()
	s, err := lc.Status(ctx)
	if err != nil {
		return failure(err)
	}
	result := map[string]any{"ok": true, "state": s.BackendState, "elapsedMs": time.Since(e.started).Milliseconds(), "path": "unknown", "health": s.Health}
	return encoded(result)
}

func checkConnection(origin string) string {
	e, err := current()
	if err != nil {
		return failure(err)
	}
	binding, err := savedTarget(e.dir, origin)
	if err != nil {
		return failure(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 8*time.Second)
	defer cancel()
	if err := e.verifyTargetTLS(ctx, binding); err != nil {
		return failure(err)
	}
	return encoded(map[string]any{"ok": true})
}

//export p2pConfigure
func p2pConfigure(snapshot *C.char) *C.char {
	return C.CString(configureInterfaces(C.GoString(snapshot)))
}

//export p2pStart
func p2pStart(dir *C.char) *C.char { return C.CString(start(C.GoString(dir))) }

//export p2pStatus
func p2pStatus() *C.char { return C.CString(status()) }

//export p2pEnroll
func p2pEnroll(token *C.char, raw *C.char) *C.char {
	permit, err := nativeOperations.claim(C.GoString(token))
	if err != nil {
		return C.CString(failure(err))
	}
	e, err := current()
	if err != nil {
		return C.CString(failure(err))
	}
	result, err := e.enroll(C.GoString(raw), permit)
	if err != nil {
		return C.CString(failure(err))
	}
	return C.CString(string(result))
}

//export p2pCancelEnrollment
func p2pCancelEnrollment() *C.char {
	if err := nativeOperations.revokeAll(); err != nil {
		return C.CString(failure(err))
	}
	e, err := current()
	if err != nil {
		return C.CString(encoded(map[string]any{"ok": true}))
	}
	if err := e.cancelEnrollment(); err != nil {
		return C.CString(failure(err))
	}
	return C.CString(encoded(map[string]any{"ok": true}))
}

//export p2pResetEnrollment
func p2pResetEnrollment(token *C.char) *C.char {
	permit, err := nativeOperations.claim(C.GoString(token))
	if err != nil {
		return C.CString(failure(err))
	}
	e, err := current()
	if err != nil {
		return C.CString(failure(err))
	}
	if err := e.resetEnrollment(permit); err != nil {
		return C.CString(failure(err))
	}
	return C.CString(encoded(map[string]any{"ok": true}))
}

//export p2pReserveOperation
func p2pReserveOperation() *C.char {
	token, err := nativeOperations.reserve()
	if err != nil {
		return C.CString(failure(err))
	}
	return C.CString(encoded(map[string]any{"ok": true, "token": token}))
}

//export p2pConfirmLegacy
func p2pConfirmLegacy(token *C.char, origin *C.char) *C.char {
	permit, err := nativeOperations.claim(C.GoString(token))
	if err != nil {
		return C.CString(failure(err))
	}
	e, err := current()
	if err != nil {
		return C.CString(failure(err))
	}
	if err := e.confirmLegacyTarget(C.GoString(origin), permit); err != nil {
		return C.CString(failure(err))
	}
	return C.CString(encoded(map[string]any{"ok": true}))
}

//export p2pCancelOperation
func p2pCancelOperation(token *C.char) *C.char {
	if err := nativeOperations.revoke(C.GoString(token)); err != nil {
		return C.CString(failure(err))
	}
	return C.CString(encoded(map[string]any{"ok": true}))
}

//export p2pTailnet
func p2pTailnet(origin *C.char) *C.char {
	e, err := current()
	if err != nil {
		return C.CString(failure(err))
	}
	proxy, err := e.bindTailnet(C.GoString(origin))
	if err != nil {
		return C.CString(failure(err))
	}
	return C.CString(encoded(map[string]any{"ok": true, "proxy": proxy}))
}

//export p2pDirect
func p2pDirect(origin *C.char) *C.char { return C.CString(configureDirect(C.GoString(origin))) }

//export p2pStopDirect
func p2pStopDirect() { stopDirect() }

//export p2pCheck
func p2pCheck(origin *C.char) *C.char { return C.CString(checkConnection(C.GoString(origin))) }
func main()                           {}
