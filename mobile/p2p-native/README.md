# P2P phone trial

Uses tailscale.com/tsnet v1.102.3 (BSD-3-Clause and its transitive licenses).
Build a C shared library for Android ARM64 with Go's Android target and NDK29.
The JNI wrapper only exposes node start, status, one fixed HTTPS probe and the
WebView proxy address. It never registers Android VpnService.

First-run enrollment is interactive through the device's external browser.
Node keys remain in the separate trial application's private files. Uninstalling
this trial removes its local keys; its tailnet device can be removed separately.
No admin API credentials/auth keys are compiled into the application.

This is a user-requested prototype, not a reviewed release. See
../../docs/mobile-p2p-trial.md for scope and the later review gate.
