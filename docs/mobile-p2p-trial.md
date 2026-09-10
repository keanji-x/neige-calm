# Android userspace connectivity trial

User-authorized prototype: deliver an installable APK after a small compile,
packaging and focused boundary smoke check. Phone testing precedes dual review,
full CI and merge. This branch is not production-ready and must not be merged
before that later process.

Use the official tsnet userspace stack inside the app process. Do not declare or
start Android VpnService, change device routing, or disconnect the user's VPN.
The existing VPN may still carry the app's UDP/TCP traffic; direct connectivity
is measured, never assumed. Use interactive Tailscale device enrollment, store
node keys in app-private files, and embed no enrollment or server credentials.

The trial has a separate application ID and a native startup screen for login,
connection status, a real /api/version latency probe and opening the existing
bundled frontend. A WebView-only loopback CONNECT proxy dials the existing
server's tailnet IP through tsnet while retaining its original HTTPS hostname,
TLS verification, cookies, QR pairing and native capability restrictions.
Only pivot-neige.tail328551.ts.net:10000 is accepted by the proxy; no arbitrary
forwarding or direct-network fallback. The existing dedicated mobile router
remains authoritative. No server deployment or Funnel changes are required.

Acceptance for phone handoff: ARM64 build, no VPN service/permission in merged
manifest, correct TLS/no arbitrary CONNECT target, no embedded auth keys, actual
bundled frontend and original signing key verified. Report untested Android
runtime/VPN coexistence clearly; the user will validate those on their phone.
