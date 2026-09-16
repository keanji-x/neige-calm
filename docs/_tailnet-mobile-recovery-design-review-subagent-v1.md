> 归档说明：独立 subagent 对 v1 的原始评审；仅静态设计/源码检查及所述库行为实验，未执行实际 App 或设备验收。以下原文保留。

# Independent design review A — v1

Verdict: **REQUEST_CHANGES**

Reviewed read-only against `c442f3440dc5ab819eb82eca0c43aeec8a29429d` in `/tmp/neige-recovery-design-review`.
Design SHA-256: `81f8ede3fb363cd7a1c89aee8b661004e6870ab0f469a2b4de281fe7b69243d9`.
Scope: consequential design decisions and boundary contracts, not line-by-line implementation planning. No source/design changes or mutation work were performed.

## 1. [P1] Declare the complete write fence and refusal-before-queue contract

**Design location:** `docs/architecture/tailnet-mobile-recovery.md:95-106`, `:115-118`, `:139-143`.

Keeping the authorized router mounted while suspending writes is viable, but the design only says the coordinator distributes permission. It does not identify where permission is enforced or account for writes already deferred by existing owners. This matters even if every visible button is disabled: a mutation queued while offline can begin after the recovery permission becomes true, without another user action. A transport check made only when the queued request finally starts would admit that stale intent.

**Source evidence:**

- `fe/web/src/app/auth/production-app.tsx:44-69` creates one shared REST transport, which is a practical mandatory enforcement boundary.
- `fe/web/src/app/providers/queries.ts:97-104` already explains why selected interactive writes use `networkMode: 'always'` plus a synchronous local refusal: reconnect must not submit a cancelled form later. This is not applied across all mutations. `useTodayReportResetMutation` at `:772-784` and recipe removal at `:935-936` use the default mutation network policy. Planner writes and uploads at `:344-345`, `:362-363`, and `:431-432` are plain promises, outside any universal mutation-options policy.
- Terminal input bypasses that transport: `fe/web/src/systems/terminal/xterm-view.tsx:975-984`; its local `connectionReady` is not a workspace recovery authorization. Owner claims/resize also use the same terminal sender (`:579-584`, `:707-709`, `:1062-1072`).
- A bounded experiment using the installed `@tanstack/query-core` confirmed the default queue behavior: offline `mutate()` resulted in `{writes:0, paused:true}`; changing online state to true, without another click, resulted in `{writes:1, paused:false}`. This was a library behavior check, not a production regression test.

**Minimum revision:** Name the two enforcement boundaries: the shared REST command/transport path and the terminal send/attach owner. Specify that write intent must be admitted under the current recovery generation before a mutation/serial queue accepts it, and rechecked before transmission; denied or invalidated intent is rejected, never deferred until recovery. Explicitly sweep/cancel existing paused mutation and serial-writer paths, and distinguish the narrow login/logout/probe operations required to recover from business writes. Add one acceptance row covering an offline queued mutation becoming online after the new generation is ready, plus terminal input from a still-open socket during identity revalidation. This can be a short contract paragraph; it does not require listing every button or implementing an offline queue.

## 2. [P1] Close the offline sign-out → intentional sign-in/pairing transition

**Design location:** `docs/architecture/tailnet-mobile-recovery.md:99-101`, together with S1's no-server-upgrade promise at `:14` and the no-bootstrap default at `:69-70`.

The proposed sign-out marker correctly prevents a residual cookie from automatically reopening the workspace, but its owner/scope, failure behavior and clearing transition are unspecified. In particular, the current mobile pairing path supplies no frontend-visible successful-login receipt. Keeping the marker blocks a successful new pairing; clearing it on an ordinary `/next/` arrival or any successful whoami reopens the residual-cookie path the marker is meant to prevent. This needs a deliberate policy rather than being left to two independent native/FE implementations.

**Source evidence:**

- `fe/web/src/app/auth/production-app.tsx:70-74` currently waits for the logout attempt and then reloads; it does not have a local logout state. The requested behavior therefore needs a new transition, not reuse of an existing safe path.
- `mobile/src-tauri/gen/android/app/src/main/java/io/neigecalm/next/RememberedSession.kt:8-23` persists `calm-session` as HttpOnly. It is deliberately unavailable to the FE, and the native cookie-presence hint is not proof of a new login.
- `crates/calm-server/src/mobile_access/pair.js:39-41` receives pairing success and only navigates to `/next/`. That is the same entry used by ordinary launcher resume (`mobile/www/app.js:60-64`). The pairing page is served by the server (`mobile_access/routes.rs:180-184`), not the offline bundled `/next/` frontend.
- `mobile_access/routes.rs:155-164` sets the new HttpOnly cookie; `fe/core/api/auth.ts:5-10` exposes no session generation/id in whoami. A FE cannot infer that a new pairing happened by comparing the fixed owner identity.

**Minimum revision:** Define how an explicit user sign-in/pairing action grants a bounded new attempt, what proves success, and when the sign-out marker is cleared; failure/cancellation/process death must keep automatic resume blocked. Choose a small mechanism consistent with S1: for example a bounded native cookie-clear plus explicit re-entry flow, or a narrowly scoped explicit pairing-success receipt. Do not introduce persistent server sessions or a general native configuration bridge for this. State how a failed marker write avoids falsely claiming a durable local logout. If offline logout is deliberately excluded from S1, narrow that slice explicitly while preserving the existing distinct logout behavior; do not interpret ordinary app close as logout.

## 3. [P2] Replace S4's fixed-target safety boundary with an explicit destination contract

**Design location:** `docs/architecture/tailnet-mobile-recovery.md:170-172`.

“Validate structure/target” and deriving origin/peer/port from one configuration do not yet define which targets a QR/config may authorize. Today, a fixed origin and a fixed dial address are the main restriction. S4 removes that restriction and is an authority-boundary change, so the replacement rules must be reviewable before implementation. Exact-origin checks alone compare browser authorities; they do not prove a configured peer address or a DNS answer belongs to that authority or to an approved Tailnet node.

**Source evidence:**

- `mobile/www/scanner.js:43-44` accepts a parsed QR only when its origin equals the built-in `defaultServer`.
- `mobile/www/pairing-url.js:1-11` validates URL shape and rejects only a short reserved-host list; it is not a general destination validator.
- `mobile/p2p-native/main.go:27-30`, `:113-117`, `:265-267` bind the only permitted CONNECT authority to one hardcoded peer address. Turning these constants into independent strings would lose the existing binding.
- The direct proxy's hostname path uses the system resolver when dialing (`mobile/p2p-native/direct.go:60-84`, `:143`, `:166-168`); syntactic hostname checks alone do not constrain resolved destinations.

**Minimum revision:** State the accepted Tailnet target class and how the HTTPS hostname, selected peer and port are bound; define treatment of reserved/local addresses, resolution changes, TLS mismatch and redirects before forwarding any cookie or pairing secret. Keep explicit direct-IP configuration and QR-driven Tailnet selection as distinct authority decisions. Add a compact negative matrix for mismatched origin/peer, reserved or rebound destination, and cross-origin redirect. This is needed before S4, not a reason to enlarge S1/S2.

## Reviewed decisions that do not need expansion

- **Native profile metadata need not be sent into FE on the evidence available.** A fresh document discards private memory; cold presentation excludes private text; online identity and database epoch precede business mounting. I found no concrete private-data disclosure counterexample requiring a native→web bootstrap merely to display the local shell. The phrase “same profile” in the cursor row should not itself force an additional security identity: two profiles reaching the same origin, fixed owner and same process epoch do not create distinct server principals.
- **Cold bootstrap is achievable without waiting for Tailnet.** The required local resource interceptor plus already-bound, fail-closed loopback listener is a sufficient design direction. The implementation must actually split listener availability from `node.Start()` (currently `main.go:95-109`), which the prescribed startup order already requires.
- **Route recovery fits the bundled frontend.** `createAppRouter` uses ordinary browser history and `/next` basepath (`fe/web/src/app/router/public.tsx:1248-1253`); its routes include Today, Track, Recipes and Settings (`:1159-1244`). The hash used in `navigation.ts` is a block anchor, not the primary page route. Dropping it is an explicit scope limitation, not evidence that all routes will be lost.
- **The major native lifecycle and event requirements are already stated:** retryable start failure, network callback/poll, generation checks including proxy callback, cancellable JNI deadlines, one event driver and new-epoch cursor reset. Implementation review must verify the shared whoami owner also replaces/coordinates the driver's existing independent whoami probe (`app/composition.ts:45-51`), rather than creating two concurrent probes; no separate new architecture is necessary.
- **S3 direction is sound:** restricted `public_mobile_router`, independent process, explicit environment, private state, protected local control, no public Funnel migration, single state writer and tested rollback pair. Existing Funnel and Tailnet must not share the mutable provider control/pairing origin inadvertently, but the document already requires independent provider control state; it need not specify every struct to be acceptable.
- **Architecture/key discipline is stated sufficiently:** core keys, ports, ownership change requests, real generators and narrow gates are explicitly required. No new broad allowlist or persistent identity system is needed to make this design work.

## Minimal path to convergence

Add three short contracts for write admission/no replay, sign-out/re-entry, and S4 destination binding. Preserve the existing slice split and cold-shell/hot-page distinction. Re-review the revised static document; do not expand the design into per-function pseudocode or a persistent offline-authentication subsystem.

Verification performed: repository/instruction/source inspection and the bounded installed-library mutation-queue behavior experiment above. No app tests, device checks, network calls, or source mutations were run; this is design approval only.
