# S5 Host Enrollment

## Review Fix Round 2

Both complete independent reports against `21c6ea650` requested changes. This
round addresses four distinct findings, preserving those reports and their
private probes unchanged:

- B1: disabled-host status now invokes a read-only `--cleanup-status` helper
  mode. This mode reads only the private ledger, not issuer configuration or
  credentials; it neither starts tsnet/listeners nor rewrites state. The
  periodic `--cleanup-only` process returns a bounded versioned secret-free
  report. Manager failures remain visible beside the retained record/expiry
  information. Settings polls this status even when access is disabled.
- B2: ledger v2 stores receipt-time evidence only after actual cloud key
  capabilities/lifetime passed validation. Same-binding cleanup records may
  retire by expiry only after both the returned expiry and a full provider
  lifetime plus five seconds on Linux CLOCK_BOOTTIME. A new boot restarts the
  conservative waiting window; forward wall time alone cannot retire a key.
  Unknown outcomes, mismatched configurations, missing/corrupt evidence, and
  v1 records remain retained. Expiry retirement is explicitly not a successful
  DELETE or device removal. Counts remain bounded at 64 key records.
- A1: durable admission/cancellation fences are distinct from key records and
  survive successful deletion. Cancellation before a create arrives records
  its ID first; any replay inside the still-admissible command window fails
  before OAuth/key POST. Fences cover the ten-second maximum control deadline,
  require wall and boot-time passage to prune, and cap at 128 entries. A full
  fence set refuses new admission/cancellation rather than acknowledging an
  unrecorded cancellation. Unknown-result records remain an independent fence.
- A2: an observed authoritative disable or confirmed origin/node change
  advances the Settings generation and clears the QR. Late create responses
  are cancelled, including when the server has already been re-enabled.
  Offline/degraded status with no new authoritative identity is not revocation.
  Cleanup status no longer remains hidden behind an old local cancel response.

Public claim/redeem and management DTOs, REST 9 and web 30 remain unchanged.
Ledger migration is explicit: old six-field v1 records acquire **no** invented
expiry evidence. No deployed migration was changed. The bounded CLI report is
internal to the manager/helper; old helpers fail explicitly and require a full
host restart after update.

Red reproductions are under `/tmp/neige-1712-s5-r2-*-red.log`: expiry capacity,
real stopped host, UI disabled status, both control replay cases, and both UI
revocation cases. The first stopped-host test attempt failed while replacing
an existing script with a Go executable; its corrected run reached the actual
disabled-status rejection before any root fix. The final integration test also
sends a partial create over the real app socket, lets cancellation overtake it,
then finishes the original request against the production Go control server
with only node/API I/O faked. No cloud POST is allowed in that sequence.

Round-2 source checkpoint:
`a0066ea13e5a7a2a930154bcd91747eb0691b5b9`. The final report commit changes only
this document. Fresh full A and CLI B review of the updated source remains
required; the initial review reports are not approvals of this checkpoint.

### Round-2 Verification

All final Rust commands retained `NEIGE_CODEX_BIN` unset, `RUSTC_WRAPPER=` and
`CARGO_BUILD_JOBS=4`, with `CARGO_TARGET_DIR=/tmp/neige-1712-s5-target`. Parent
explicitly confirmed CLI B had finished and released that target before work
resumed. Go used `GOMAXPROCS=2 GOCACHE=/tmp/neige-1712-s5-go-cache` and `-p 2`.

| Executed check | Result | Log under `/tmp/` |
| --- | --- | --- |
| `cargo nextest run --locked -p neige-app -p calm-tailnet-control -E 'test(tailnet)' --test-threads 2 --no-fail-fast` | 15 passed, including actual helper/status and app-to-Go reorder | `neige-1712-s5-r2-host-green.log` |
| Same 48-test focused server selection as the initial round below | 48 passed on restored round-2 source | `neige-1712-s5-r2-server.log` |
| `scripts/local-rust-gates.sh --quick` | all five checks green; broad nextest skipped | `neige-1712-s5-r2-quick.log` |
| `go test -race -p 2 -count=1 -json ./...` | 28 top-level passed, 1 subprocess-only fixture skipped | `neige-1712-s5-r2-go-default.jsonl` |
| Same Go command with `-tags ts_omit_logtail` | 29 top-level passed, 1 subprocess-only fixture skipped | `neige-1712-s5-r2-go-tagged.jsonl` |
| `npm run gen:api` | 89 export tests passed, OpenAPI emitted, no public generated-file drift | `neige-1712-s5-r2-generator.log` |
| `npm run lint` and `npm run build` | green, existing large-chunk warning only | `neige-1712-s5-r2-fe-lint.log`, `neige-1712-s5-r2-fe-build.log` |
| `npm test -- --maxWorkers=2` | 3292 passed, 1 skipped | `neige-1712-s5-r2-fe-test.log` |
| `npx vitest run --project browser web/src/app/shell/enrollment-revocation.browser.test.tsx web/src/features/settings/tailnet.browser.test.tsx --maxWorkers=2` | 8 passed | `neige-1712-s5-r2-browser.log` |

The skipped Go fixture is intentionally started as the real helper subprocess
by `tailnet_app_cancel_overtakes_partial_create_without_cloud_post`; it is not
an untested production path. Its fake node/API are confined to `_test.go` and
the production issuer, ledger, control protocol and Rust manager are exercised.
The stopped-host test builds the actual production Go executable, verifies the
real v2 status response/expiry, checks that status leaves ledger bytes intact,
and checks that corrupt data produces an error rather than zero pending keys.

Nine single-factor production mutations completed on the clean checkpoint.
Every full observed red set matched its prediction, original byte hashes were
restored, and all selected tests were green after each restoration. Exact
plans, red/green logs and results:
`/tmp/neige-1712-s5-r2-mutations/{plan,evidence}.json` and its adjacent logs.

| Mutation | Exact failing set |
| --- | --- |
| disable expiry retirement | expiry-capacity test plus clock test and its four named subtests |
| remove elapsed boot-time requirement | clock test and only its `wall-forward` subtest |
| bypass configuration binding | original binding test plus retained-record test and its `binding` subtest |
| bypass cleanup-state guard | retained-record test and only its `unknown` subtest |
| bypass durable cancellation fence | two Go control replay tests; independently, only the real Rust app-handler reorder test |
| reject stopped status again | actual disabled-host status and cleanup-process-failure tests |
| stop disabled polling | only the disabled cleanup/actual-expiry UI test |
| keep QR object on invalidation | external disable/re-enable, origin-change and node-change UI tests |
| keep old UI generation | only the delayed-create-after-re-enable UI test |

The complete named sets are in `plan.json`; no aggregate red count substitutes
for them. Temporary mutation work is finished and the author tree was clean
before this report update. No independent reviewer observed transient mutations.

Exact tagged helper: `/tmp/neige-1712-s5-r2-helper`, built in clean standalone
clone `/tmp/neige-1712-s5-r2-artifact-source`. The real artifact verifier passed;
`go version -m` reports the exact checkpoint above, `vcs.modified=false`, and
`ts_omit_logtail`. Build info: `/tmp/neige-1712-s5-r2-helper-buildinfo.txt`.
SHA-256: `51b60e8133d51a7696e7a5e53266c60b10a6ea5634dbc48cc12fd7375fa8f77d`.

No actual credentials, account/API keys, devices, system Tailscale, ACLs or
other worktrees were operated on. Real cloud expiry and phone acceptance
remain unverified release blockers. This round changes no public DTO, so it
adds no ownership trailers beyond the three initial trailers listed below.

Implementation base: `3757477219263c3e9b887077473c384762545336`.
Worktree: `.claude/worktrees/tailnet-enrollment`, sole writer. No rebase onto
the subsequently fetched main, account/device operation, deployment or push.
Independent A and CLI B implementation reviews are owned by the parent task.

## Contracts

- Existing S3 control v1 remains separate. Enrollment control is v2, with
  strict bounded messages, request deadlines and generation IDs. An old helper
  fails explicitly; replacing its binary requires a full neige-app restart.
- Long-lived credentials are read only inside Go from a typed private file.
  Every directory component is opened without following symlinks; the final
  parent must be owner-only 0700 and each regular file owner-only 0600 with one
  link. Neither Rust nor Settings receives the OAuth secret or access token.
- Official HTTPS OAuth/key endpoints are fixed. No credential redirects,
  environment proxies, ACL changes, or device APIs. The current embedded
  node must match expected tailnet/origin, have HTTPS/ingress ready, and report
  Tailnet Lock disabled. System Tailscale state is not used.
- Key creation requests 300 seconds and verifies actual created/expires,
  explicit returned capabilities and exact phone tags. Invitation lifetime is
  at most 180 seconds and cannot outlive the real key expiry. Missing boolean
  capability fields are rejected, not assumed false.
- A bounded durable unknown-result marker precedes the key POST. Once an ID
  arrives it is persisted with the returned expiry before grant validation.
  Failed/uncertain creation is never automatically repeated. Unknown IDs and
  configuration-mismatched records remain for administrator reconciliation.
- Cleanup records bind to the canonical issuer configuration, including the
  expected tailnet, client ID, secret path, tags and origin. A new binding's
  404 cannot erase an old binding's record. Cleanup-only runs without starting
  tsnet, a listener or node state. Disabled hosts periodically run this mode.
- Rust v2 invitations are separate from v1. Claim hashes the ticket/attempt
  secret, binds the first attempt, and allows only identical attempt retries.
  Redeem creates one PairedDevice session under the existing grant lock;
  retries return only that still-live session. Revoke/disable/cancel invalidate
  retries. Creation completion is fenced against late cancellation/disable.
- Owner management is PasswordLogin-only, including direct main-port access;
  dev_autologin and PairedDevice sessions cannot manage enrollment. Public
  ingress exposes only claim/redeem, never management. Responses are no-store.
- Settings holds QR only in component memory, drops it at its absolute deadline,
  cancels a late response after unmount, and never auto-retries creation.
  Cleanup state distinguishes cloud-key cleanup from removing a joined device.
- The fixed claim/redeem DTOs and seven-field `neige-enroll:v2:` envelope match
  the approved handoff. No receipt/callback API. REST revision is 9 and web compatibility is 30;
  final S4 integration must rebuild its bundled frontend against this contract.

## Local Configuration

This is deployment configuration, not a command to modify an existing account.
Do not paste credentials into chat, argv, environment variables or logs.

```toml
[tailnet]
provider = "private-tailnet"
enrollment_config = "/absolute/private-directory/tailnet-enrollment.json"
```

The directory must be owned by the Neige user with mode 0700. Configuration and
secret files must be regular, non-symlink files owned by that user with mode
0600. Use canonical absolute paths without symlinked ancestors.

```json
{
  "schemaVersion": 1,
  "clientId": "ADMIN_PROVIDED_CLIENT_ID",
  "secretFile": "/absolute/private-directory/tailnet-oauth.secret",
  "phoneTags": ["tag:neige-phone-test"],
  "expectedTailnet": "ACTUAL_CURRENT_TAILNET_NAME",
  "expectedOrigin": "https://ACTUAL_EMBEDDED_NODE.ts.net"
}
```

Use the embedded node's actual CurrentTailnet.Name, never `-`, and its actual
canonical HTTPS origin. The administrator provisions only auth_keys Write and
the fixed phone tags, and separately authorizes their minimum access policy.
Neige does not modify tag ownership, grants, device approval or Tailnet Lock.

`enrollment-ledger.json` is secret-free but security-relevant. An unknown result
requires administrator reconciliation against the original account. Do not
delete or rewrite it simply to enable another attempt. Lost key IDs cannot be
recovered by blindly repeating the create request.

## Initial Verification Record

Initial implementation/test source was
`ee6c43934382bce56f0dc00462a2ee840c39af44` (2026-09-18). The subsequent report-only
commit does not change executable source. All Rust commands used
`env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=4
CARGO_TARGET_DIR=/tmp/neige-1712-s5-target`; all Go commands used
`GOMAXPROCS=2 GOCACHE=/tmp/neige-1712-s5-go-cache` for final runs.

| Executed check | Final outcome | Log under `/tmp/` |
| --- | --- | --- |
| `cargo nextest run --locked -p calm-server --lib --test domain_api_suite --test kernel_process_suite -E 'test(mobile_access::) \| test(mobile_pairing) \| test(deferred_write_tx_invariant) \| test(version::)' --test-threads 4 --no-fail-fast` | 48 passed | `neige-1712-s5-rust-final.log` |
| `cargo nextest run --locked -p neige-app -p calm-tailnet-control -E 'test(tailnet) \| test(mcp_setup_contract_revision)' --test-threads 4 --no-fail-fast` | 12 passed; no test matched the extra preflight filter | `neige-1712-s5-control-app-final.log` |
| `scripts/local-rust-gates.sh --quick` | fmt, feature clippy, default lib check, release build, OpenAPI drift green; broad nextest intentionally skipped | `neige-1712-s5-quick-gates.log` |
| `npm run gen:api` in `fe` | 89 binding exports passed; real OpenAPI generator ran | `neige-1712-s5-generate.log` |
| `npm run lint` in `fe` | green, including exact-file ownership trailers | `neige-1712-s5-fe-lint.log` |
| `npm run build` in `fe` | green; existing large-chunk warning remains | `neige-1712-s5-fe-build.log` |
| `npm test -- --maxWorkers=2` in `fe` | 3285 passed, 1 skipped | `neige-1712-s5-fe-test.log` |
| `npx vitest run --project browser web/src/features/settings/tailnet.browser.test.tsx --maxWorkers=2` | 2 passed; 390/1280 width, loaded QR and working cancellation | `neige-1712-s5-browser.log` |
| `go test -race -p 2 -count=1 -json ./...` in `tailnet` | 21 top-level tests passed | `neige-1712-s5-go-default.jsonl` |
| `go test -race -tags ts_omit_logtail -p 2 -count=1 -json ./...` | 22 top-level tests passed | `neige-1712-s5-go-tagged.jsonl` |
| `bash scripts/gate-web-compat-version-lockstep.sh` | both declarations are 30 | terminal output |

The FE dependencies were installed with `npm ci --ignore-scripts`; no lockfile
changed. Browser screenshots are in `fe/test-results/enrollment-settings-*`.
Both complete QR panels were visually inspected. The QR image is generated by
the same Rust qrcode library using synthetic non-credential text. Tests do not
present this image as an actual enrollment or as a successful device scan.

Initial sandbox runs failed at local socket binding or Git fixture subprocesses
(`EPERM`). They were rerun with approved local fixture permissions. The final
results above come from completed reruns, not those partial failures. The first
quick gate found a collapsible-if lint, subsequently fixed and rerun green.
Explicit version tests were updated from their old literals to REST 9/web 30.

Three production-boundary regressions were reproduced before their fixes:
`neige-1712-s5-capability-red.log` (missing boolean capability),
`neige-1712-s5-ledger-restart-red.log` (orphaned temporary ledger blocked restart),
and `neige-1712-s5-key-id-red.log` (case-folded ID bypassed durable metadata).

### Mutation Evidence

Every mutation changed one production factor using apply_patch on a clean
committed exclusive tree, checked that it applied, compared the complete red
set, restored the original byte hash, and reran green with no residue.

Final Go evidence: `/tmp/neige-1712-s5-go-mutations-final/evidence.json`, source
`ee6c43934382bce56f0dc00462a2ee840c39af44`. Seven mutations each had exactly one
predicted failing test, with all enrollment tests green after restoration:

- real lifetime: `TestEnrollmentRejectsLongActualLifetimeWithShortRemaining`
- configuration binding: `TestEnrollmentCleanupBindingRetainsOldNetworkRecords`
- uncertain POST: `TestEnrollmentUnknownPostIsNotRetriedAcrossRestart`
- required capability: `TestEnrollmentMissingCapabilityIsNotAssumedFalse`
- redirects: `TestEnrollmentRejectsCredentialRedirects`
- durable ID: `TestEnrollmentReturnedIDMustMatchDurableMetadata`
- interrupted write: `TestEnrollmentRestartPreservesLedgerAfterInterruptedTemporaryWrite`

Rust evidence: `/tmp/neige-1712-s5-mutations/evidence.json`, original checkpoint
`b9e50f9566bd8c22932bd91241dacf6c0ea5eff3`. That implementation commit was recreated
as `1a2bb855f73435bae57780d44da957620e27ebf7` with the identical tree to correct
ownership trailers. The original history is retained at
`refs/codex/s5-pre-ownership-trailers`. The tested production file has not changed
since these mutations. Both ran all 10 pairing unit tests with no fail-fast:

- bypass cached-session branch: only `scan_retry_returns_exactly_one_live_session` red
- bypass ticket-hash fence: only `scan_v1_and_v2_tickets_are_disjoint` red

The restored Rust file SHA-256 is
`6ecfb973783377d98e7bf3be3341454b7ae3633ab4c484023d2042fa403eaa01`.
Final focused Rust and quick gates subsequently passed. Independent reviews
have not yet run in this task; the parent must perform fresh A and CLI B reviews
of the complete `375747721...HEAD` S5 diff.

### Exact Helper Artifact

Clean standalone shared-object clone:
`/tmp/neige-1712-s5-final-artifact-source`; source commit
`ee6c43934382bce56f0dc00462a2ee840c39af44`.
Built with `go build -p 2 -tags ts_omit_logtail` and checked using
`bash tailnet/verify-build.sh /tmp/neige-1712-s5-helper-final`.
`go version -m` reports that exact commit and `vcs.modified=false`; full output
is `/tmp/neige-1712-s5-helper-buildinfo.txt`. Artifact SHA-256:
`82af522b1607aad45f604aa8057e345c06dca6722edd23bddc84a93171037550`.

### Ownership Trailers

Preserve these exact lines in the eventual PR body:

```text
OWNERSHIP-CHANGE: fe/core/api/enrollment.ts — add approved scan-only enrollment client contracts (#1712)
OWNERSHIP-CHANGE: fe/core/api/generated/openapi.json — generate approved scan-only enrollment schemas (#1712)
OWNERSHIP-CHANGE: fe/core/api/generated/wire.ts — generate approved scan-only enrollment wire types (#1712)
```

## Required External Acceptance

No existing credential files were read and no account or device calls were
performed. OAuth API acceptance of a real 300-second lifetime is unverified.
Tests use synthetic credentials and controlled HTTP responses; they are not
evidence of cloud expiry, tag scope enforcement, MagicDNS/HTTPS provisioning,
device approval behavior, or a real phone's one-scan flow. Those remain release
blockers for the parent integration task. The console's 1-90 day UI is not
treated as proof that the API accepts or clamps 300 seconds.

Go VCS metadata from this nested worktree is not source provenance. Final helper
artifact validation must use a clean standalone checkout of the committed S5
source and independently record that SHA, alongside the ts_omit_logtail build
verification. No production release is authorized by these local checks.
