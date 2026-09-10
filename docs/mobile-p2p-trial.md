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

## Remembered connection and redesigned launcher

The next prototype replaces the native diagnostic Activity with one packaged
launcher, exposing only login and QR pairing. The existing userspace node keeps
its identity in the same no-backup directory; startup automatically restores it.
Only local-launcher capabilities may read connection state, start browser login
or select the fixed server proxy. Remote pages receive none of these permissions.

With explicit user authorization to remember workspace access, the existing
calm-session cookie is retained for up to 30 days in WebView's private cookie
store with Secure, HttpOnly and SameSite=Strict intact. No cookie or enrollment
URL is exposed to launcher JavaScript or stored in localStorage. The server is
still authoritative for session validity and revocation; the cookie is only a
resume hint. Auto-resume is consumed once per WebView to prevent an invalid
cookie bouncing endlessly between the workspace and connection page. Returning
to the connection page offers pairing instead. This does not change server
session lifetime or deploy any new server configuration.

## IP-first failover prototype

Persist an independently configured direct IP/server origin, the Tailscale option,
and the last edited mode in app-private preferences. Automatic startup probes IP
first, then Tailscale, with a bounded deadline per candidate. A successful check
must return the real Neige /api/version shape; node-online alone is insufficient.
No redirects or credentials are used in probes. Failed attempts are not retried
forever; the packaged configuration homepage remains available after exhaustion.
Manual configuration changes invalidate any older attempt before it can bind.

HTTP is accepted for explicitly configured literal private or public IP addresses
(excluding loopback, link-local/metadata and multicast). HTTPS also supports
hostnames with normal certificate verification; the selected scheme is preserved. Android cleartext support is
explicitly enabled by a typed build profile field, while the active WebView client
rejects unconfigured HTTP destinations. The direct path uses an app-local, fixed-origin proxy over ordinary sockets;
the Tailscale path uses its userspace-network proxy. Neither direct traffic nor
its redirect hops use a cloud relay. Both proxy configurations remove implicit
bypass rules and allow only the packaged launcher to bypass; redirect destinations
are checked again by the proxy, not only by WebView interception. Cookies are retained separately
for each origin; HTTPS cookies are never copied to a newly entered IP. A new IP
origin may need its own workspace login once. The same backend remains authoritative
for authentication, expiry and revocation on either path.

At the root workspace, Android Back opens the configuration homepage without
logging out. Back from that homepage closes the Activity rather than returning
to stale workspace history, so persistent auto-entry never hides configuration.
