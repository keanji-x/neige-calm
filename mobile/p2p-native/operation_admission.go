package main

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"errors"
	"sync"
)

// Reserved synchronously by Java before it queues a worker, even before the
// engine exists. Cancellation and the final native commit share permit.mu.
type operationPermit struct {
	mu      sync.Mutex
	token   string
	ctx     context.Context
	cancel  context.CancelFunc
	claimed bool
	stop    func() error
}

func (p *operationPermit) revoke() error {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.cancel()
	if p.stop != nil {
		return p.stop()
	}
	return nil
}

type operationAdmission struct {
	mu      sync.Mutex
	current *operationPermit
}

func (a *operationAdmission) reserve() (string, error) {
	a.mu.Lock()
	defer a.mu.Unlock()
	if a.current != nil {
		if err := a.current.revoke(); err != nil {
			return "", err
		}
	}
	var nonce [32]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		return "", errors.New("无法创建原生操作")
	}
	ctx, cancel := context.WithCancel(context.Background())
	a.current = &operationPermit{token: hex.EncodeToString(nonce[:]), ctx: ctx, cancel: cancel}
	return a.current.token, nil
}

func (a *operationAdmission) claim(token string) (*operationPermit, error) {
	a.mu.Lock()
	defer a.mu.Unlock()
	p := a.current
	if p == nil || token != p.token {
		return nil, errors.New("原生操作已取消")
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.claimed || p.ctx.Err() != nil {
		return nil, errors.New("原生操作已取消")
	}
	p.claimed = true
	return p, nil
}

func (a *operationAdmission) revoke(token string) error {
	a.mu.Lock()
	defer a.mu.Unlock()
	if a.current == nil || a.current.token != token {
		return nil
	}
	return a.current.revoke()
}

func (a *operationAdmission) revokeAll() error {
	a.mu.Lock()
	defer a.mu.Unlock()
	if a.current == nil {
		return nil
	}
	return a.current.revoke()
}

var nativeOperations operationAdmission
