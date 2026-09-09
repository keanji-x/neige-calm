# Mobile QR pairing: first complete slice

## Outcome

An authenticated owner opens Next Settings → Network → Mobile connection,
starts public HTTPS access, and creates a pairing QR. The Android Neige client
scans it, shows a verification code, and waits for approval in the authenticated
desktop page. Approval creates a separate revocable Neige session on the phone;
the phone enters `/next/` without installing Tailscale or entering a password.

The user's host has no public IP. Funnel provides NAT traversal through its
public relay/HTTPS name; the Neige ingress binds loopback only. No router port
forwarding, publicly bound listener, or user-owned public IP is required.

This slice integrates an already installed and authorized server-side Tailscale
daemon using Funnel. Account enrollment and machine installation are explicit
deployment prerequisites, not privileges delegated to a browser. The default
configuration disables remote access. Deployment addresses and daemon sockets
stay in local configuration and never enter source control.

## Authority and lifetime

- Opt in with a typed `--mobile-access-config` JSON file. Required fields identify
  an absolute Tailscale executable, its daemon socket, and an unused HTTPS port
  from Funnel's supported set. Validate them at boot. No implicit environment
  configuration or arbitrary command/target supplied by a browser.
- Build child environments from an explicit allowlist. Serialize controller
  operations, bound command output/time, and redact provider errors before
  returning them to the UI. Never reset or overwrite another Serve/Funnel mapping.
- Use a foreground Funnel process targeting a dedicated loopback ingress owned
  by this server. Explicit stop/server shutdown terminates only that process.
  Do not advertise a QR until the provider reports the intended public mapping.
  Linux parent-death signaling also terminates the helper after SIGKILL/crash;
  setuid/setgid wrappers are refused. Normal shutdown adds at most one second to
  the existing three-second drain, within the supervisor's five-second grace.
- The dedicated ingress uses the existing protected REST/WS and public auth
  surfaces, but never registers local worker hooks or remote-access management
  routes. Reuse the production route assembly and static-file setup. A loopback
  peer created by the reverse proxy is not evidence of a trusted worker.
- Refuse public access with dev autologin or missing owner credentials. Management
  requests require a real owner session, including after a server-side config
  change. Authentication failures are never interpreted as an empty configuration.
- Pairing tickets are high-entropy, short-lived, bounded in count, and single-use.
  A scan claims a ticket and receives a separate secret; the owner must approve
  the displayed matching code. Redemption atomically consumes the approved claim.
  No password or reusable session credential appears in a QR or URL query.
- Bootstrap URLs carry the ticket in the fragment. The bootstrap page removes
  it from history, exchanges it in POST bodies, sets a Secure/HttpOnly session
  cookie on the server origin, and then navigates to Next. No cross-origin cookie
  injection or remote Tauri permissions are needed.
- Reuse the existing in-memory session lifetime. Paired devices can be revoked;
  disabling access revokes their sessions and pending invitations. Server restart
  requires a new scan, matching existing session invalidation. Durable device
  credentials are a separate future change.
- Revocation also cancels the public transports, including already-upgraded
  WebSockets. Other paired devices reconnect with still-valid sessions. Provider
  exit invalidates invitations and sessions and closes the ingress immediately.
- The public ingress does not offer password login; authorization comes from the
  owner-approved QR exchange. Local password login remains on the existing local
  listener. Only whoami/logout are shared with the public auth surface.
- Android camera capability is limited to the packaged launcher. QR parsing is
  strict and versioned; show the target hostname before connecting. Remote pages
  retain no native capabilities. Cancellation/errors preserve manual connection.

## Frontend boundaries

The settings feature remains presentational. A Network row opens a mobile
connection dialog; app-layer hosts own transport and polling. Required props and
API schemas are updated across callers/tests. New HTTP DTOs are included in the
real OpenAPI generator and every generated consumer. Global styles and unrelated
settings contracts remain unchanged.

## Acceptance and review

1. Drive production HTTP routers for create → claim → approve → redeem → Next
   whoami → revoke. Check expiry, wrong secret, replay, concurrent redemption,
   unauthenticated management, dev-autologin refusal, and bounded state.
2. Use the real public-ingress assembly to prove local worker hooks cannot be
   reached through the reverse proxy, including spoofed headers.
3. Exercise provider command execution with a controlled executable fixture:
   exact arguments/environment, occupied-port refusal, failures, and teardown.
4. Test/preview the web settings and pairing screens in Chromium. Test actual QR
   payload generation/decoding and the Android launcher parser; build the APK.
5. A live Funnel smoke check uses a disposable authenticated server with synthetic
   data, never the existing production instance. If account policy authorization
   is missing, report it and keep provider-fake tests distinct from live evidence.
6. Mutation-verify the small set of authentication/replay/ingress assertions in
   an exclusive worktree; restore production bytes and verify green afterward.
7. Two isolated review channels, fix all in-scope findings, rerun invalidated
   checks, and merge only when required CI is green.

## Deployment

This first slice runs on a Linux server. Install Tailscale on that server, sign
in, and authorize HTTPS/Funnel for the node. No Tailscale client is needed on the
phone. Existing daemon mappings are preserved; choose an unused supported port.

Store a private configuration file outside the repository:

```json
{
  "executable": "/usr/bin/tailscale",
  "socket": "/var/run/tailscale/tailscaled.sock",
  "httpsPort": 10000
}
```

Add `--mobile-access-config /absolute/path/mobile-access.local.json` to the
server's existing arguments and provide `--fe-dist /absolute/path/fe/web/dist`.
Keep normal owner credentials configured and dev autologin disabled. For
`neige-app`, the existing `[child].extra_args` array can carry the two CLI tokens.
The daemon socket must permit the server user to create its foreground mapping;
account/administrator setup remains an explicit deployment prerequisite.

Open Settings → Network → Mobile connection → Enable → Create QR code. In Neige
for Android, choose 扫码连接, confirm the displayed hostname, and request pairing.
Compare the six-digit code on both screens and approve on the owner page. The
phone then enters the real Next workspace. Revoke removes its session and closes
live streams. Other phones reconnect; Disable removes all mobile grants and the
owned tunnel. After a server restart, enable access and pair again.

## Repeatable small-loop check

Build the actual server and Next frontend, then run:

```sh
cd mobile
npm ci
npm run test:pairing -- /absolute/path/calm-server /absolute/path/fe/web/dist
```

The test creates a private temporary database, runs the real server with real
login and pairing handlers, decodes the actual rendered QR image, and uses
separate owner/phone browser contexts. A loopback TLS byte proxy and the controlled
Funnel CLI fixture stand in for the external provider. It checks approval,
Secure/HttpOnly/SameSite cookies, Next login, replay refusal, revocation, shutdown,
and abrupt server death. It never starts a real Codex or Claude process. The
self-signed test certificate is trusted only by that isolated browser context.
This is protocol/browser evidence; it is not proof of live Funnel connectivity
or physical Android camera behavior. Native APK compilation and scanner tests
are reported separately.

## Ownership decision for issue 1598

The orchestrator approves additive changes to `fe/core/api/mobile-access.ts` and
the generated `fe/core/api/generated/openapi.json` / `wire.ts` for this slice.
They add the mobile contracts without changing existing API semantics. Matching
`OWNERSHIP-CHANGE` trailers accompany every commit touching those frozen paths.
