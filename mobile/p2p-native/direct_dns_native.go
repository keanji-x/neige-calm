//go:build (android || linux) && cgo

package main

/*
#cgo LDFLAGS: -ldl
#include <stdlib.h>
#include "direct_dns_native.h"
*/
import "C"

import (
	"context"
	"errors"
	"fmt"
	"time"
	"unsafe"

	"golang.org/x/net/dns/dnsmessage"
	"golang.org/x/sys/unix"
)

// A wedged vendor admission must not accumulate unbounded cgo calls when
// multiple proxy requests time out. Slots remain owned until admission exits.
var androidDNSAdmissions = make(chan struct{}, 2)

func verifyAndroidDirectAbsence(ctx context.Context, family, host string) error {
	typeID := dnsmessage.TypeA
	if family == "ip6" {
		typeID = dnsmessage.TypeAAAA
	} else if family != "ip4" {
		return errors.New("invalid DNS address family")
	}
	ctx, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	if err := ctx.Err(); err != nil {
		return err
	}
	select {
	case androidDNSAdmissions <- struct{}{}:
	case <-ctx.Done():
		return ctx.Err()
	}
	// Admission may involve vendor IPC. The caller remains bounded, and a late
	// descriptor has exactly one cleanup owner even after cancellation.
	ready := make(chan int)
	go func() {
		defer func() { <-androidDNSAdmissions }()
		name := C.CString(host)
		fd := int(C.neige_dns_query(name, C.int(typeID)))
		C.free(unsafe.Pointer(name))
		select {
		case ready <- fd:
		case <-ctx.Done():
			if fd >= 0 {
				C.neige_dns_cancel(C.int(fd))
			}
		}
	}()
	var fd int
	select {
	case <-ctx.Done():
		return ctx.Err()
	case fd = <-ready:
	}
	if fd == -int(unix.ENOSYS) {
		return errors.New("Cannot verify empty DNS answers on this Android resolver. Use a literal IP, Tailnet, or Android 10+ with supported DNS APIs")
	}
	if fd < 0 {
		return fmt.Errorf("Android DNS query failed: %w; use a literal IP or Tailnet", unix.Errno(-fd))
	}
	consumed := false
	defer func() {
		if !consumed {
			C.neige_dns_cancel(C.int(fd))
		}
	}()
	poll := []unix.PollFd{{Fd: int32(fd), Events: unix.POLLIN}}
	for {
		if err := ctx.Err(); err != nil {
			return err
		}
		n, err := unix.Poll(poll, 25)
		if errors.Is(err, unix.EINTR) {
			continue
		}
		if err != nil {
			return err
		}
		if n == 0 {
			continue
		}
		if poll[0].Revents&unix.POLLIN == 0 {
			return errors.New("Android DNS response channel failed; use a literal IP or Tailnet")
		}
		if err := ctx.Err(); err != nil {
			return err
		}
		// A malformed/partial vendor response must not block beyond our deadline.
		if err := unix.SetNonblock(fd, true); err != nil {
			return err
		}
		answer := make([]byte, 65535)
		var rcode C.int
		length := int(C.neige_dns_result(C.int(fd), &rcode, (*C.uint8_t)(unsafe.Pointer(&answer[0])), C.size_t(len(answer))))
		consumed = true // android_res_nresult closes fd on both success and error.
		if err := ctx.Err(); err != nil {
			return err
		}
		if length < 0 || length > len(answer) {
			return errors.New("Android DNS result failed; use a literal IP or Tailnet")
		}
		return validateDirectDNSAbsence(host, typeID, int(rcode), answer[:length])
	}
}
