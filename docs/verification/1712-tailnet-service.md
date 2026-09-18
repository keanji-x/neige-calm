# #1712 S3 private Tailnet checkpoint verification

This checkpoint implements the app-owned private node, local Settings controls,
restricted Unix ingress, persistence/lifecycle and release packaging. Scan-only
phone enrollment is the next separately approved slice. No deployment, account
login, real Tailnet state, system tailscaled socket or production credentials were
used in these checks. Independent implementation reviews are coordinated by the
parent task; this document does not replace those reviews or device acceptance.

## Production-boundary checks

- The real API generator passed: `env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 npm run gen:api` from `fe` (83 binding exports, then OpenAPI).
- `env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked -p calm-server --lib --test domain_api_suite -E 'test(mobile_pairing) | test(private_tailnet) | test(auth::)' --test-threads 8`: 40 passed. This includes real restricted Unix HTTP/WS, password/worker/management route exclusion, all mobile management actions rejecting paired credentials, valid business access, and revocation closing a live WebSocket.
- `env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked -p neige-app -p calm-tailnet-control -E 'test(tailnet) | test(package_) | test(source::tests)' --test-threads 8`: 24 passed. This covers environment isolation through a real fixture child, durable disable retaining identity, private state locks/backups, crash circuit, kernel independence, pinned helper binary, protocol bounds, and hashed packaging.
- Go `go test -p 4 ./...` and `go test -race -p 4 ./...` passed using the cached Go 1.26.6 toolchain and module cache. Nine isolated tests ran for fixed Unix upstream, forwarded identity removal, live upgraded connection closure, node locks/state version, login/approval states and certificate loss. Independent review subsequently found the pending-listen fake did not use its barrier; the original run is not evidence of the claimed blocked-operation behavior. The corrected barrier test and new production mutations are recorded below. No real tsnet Server was started by tests.
- `npm ci --cache /tmp/neige-tailnet-npm-cache`, `npm run lint`, `npm run build`, and `npm test -- --maxWorkers=4` completed; the final frontend suite reports 3,282 passed and one existing skip. The initial suite found the new workflow missing the mandatory quoted npm-audit setting; that was corrected before the full green rerun.
- `npm run test:browser -- web/src/features/settings/tailnet.browser.test.tsx`: one passed in Chromium. The 390px Settings screenshot was visually inspected. `npx playwright install --with-deps chromium` could not use sudo without a password; `npx playwright install chromium` and the browser run succeeded with the existing system libraries.
- `env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 scripts/local-rust-gates.sh --quick` passed: format, workspace clippy with features, default-feature lib check, real release compilation and OpenAPI drift. No workspace-wide nextest or real Codex E2E was run. The first quick run found a collapsible condition; the condition and a fixture-only redundant format call were corrected and the complete quick gate rerun green.
- `tailnet/build.sh /tmp/neige-tailnet-artifact/neige-tailnet` built the real static Linux helper with the cached pinned toolchain; `--version` printed `neige-tailnet 0.1.0` without opening node state.
- Shell syntax, final diff whitespace and WEB_COMPAT_VERSION lockstep were checked; the maintained frontend and server both declare 29.

## Isolated final verification

A later shared-target run picked up another worktree's `calm-types` metadata and
incorrectly reported the new module missing. Final Rust verification therefore
uses `CARGO_TARGET_DIR=/tmp/neige-1712-tailnet-target`. Only external libraries and
build/fingerprint caches were copied; workspace libraries and old test binaries
were excluded. The copy contains no shared hardlinks or symlinks, and compiler
logs show all relevant workspace crates, including `calm-types`, rebuilt from this
worktree. Cargo's attempted package clean refused the copied directory's missing
cache tag without deleting anything; excluded workspace outputs already forced
the actual rebuild. The shared target was not cleaned.

With that private target and the same unset NEIGE_CODEX_BIN / cleared wrapper /
six-job cap:

- Server lib + domain tests with `-E 'test(mobile_access) | test(mobile_pairing) | test(private_tailnet) | test(auth::)'`: **42 passed**, including the existing reader-wakeup regression.
- App/control tests with `-E 'test(tailnet) | test(package_) | test(source::tests) | test(config) | test(supervisor)'`: **43 passed**. An old plugin test's empty-argv assumption was updated to explicitly expect the new private-ingress flag for fresh configurations. Explicit node state paths retain their provenance when a main-data-dir CLI override is supplied.
- The complete `scripts/local-rust-gates.sh --quick`: **passed**, including release compilation and no OpenAPI drift.

## Recorded red reproduction

The initial private-ingress test found that a real paired cookie could use the
main local port to call mobile management (expected 403, actual 200). Session
creation had no credential provenance. SessionAuthority is now required at every
creation call; management reads the current valid session and accepts only
PasswordLogin. A PairedDevice session remains non-administrative even without a
pairing-registry row. Whoami and authorized business access remain available.

## Single-factor production mutations

Each row was declared before mutation, run in an exclusive worktree, compared
against the complete actual red set in the selected suite, restored from a byte
backup, hash-checked for exact restoration, then rerun green. No test/fixture was
mutated to produce a failure. Each expected set below contains exactly one test;
the actual set matched it in every run.

| Mutation | Expected and actual failing test |
| --- | --- |
| owner-source | `mobile_pairing::private_tailnet_ingress_auth_and_control_fence` |
| backend-revoke | `mobile_pairing::private_tailnet_disable_closes_active_websocket_and_revokes_session` |
| spawn-env | `tailnet::tests::tailnet_spawn_env_allowlist` |
| fixed-upstream | `TestFixedUpstreamRejectsNetworkTargetsAndNoncanonicalSockets` |
| forwarded-identity | `TestProxyLocksUpstreamAndStripsUntrustedIdentity` |
| helper-revoke | `TestRevocationClosesUpgradedTransport` |

The selected Rust suites were the two `private_tailnet` domain tests and the
seven `tailnet` app tests; the Go suite ran all nine helper tests. Raw isolated
logs and byte backups were left under `/tmp/neige-tailnet-mutation-pq5todf2` for
this local review. A subsequent nonsemantic clippy condition fold changed the
app module formatting; final affected checks were rerun.

## Remaining acceptance

Actual desktop account authorization, tailnet ACL/MagicDNS/HTTPS behavior,
two-device connectivity and real install/upgrade/rollback exercises require a
separately authorized environment. An older running neige-app cannot parse the
new manifest unit: the first upgrade must use the new package's CLI and a full
host restart, as documented in `docs/private-tailnet.md`. Existing configurations
need the explicit provider section; existing Funnel remains separate.

## Review A corrections

The immutable S3 review identified one production defect and one evidence gap;
both are addressed in the follow-up checkpoint. The independent B process did
not produce a verdict and is not counted as an approval. Fresh reviews remain
required against the corrected snapshot.

1. `tailnet_operational_startup_failure_preserves_local_service` first failed
   against the real `neige-app system serve` entry point with an unknown desired
   schema, proving that the optional subsystem stopped the local kernel. After
   correction, unknown schema, corrupted JSON, a directory at the control-socket
   path and a regular file at the private-state path all leave the isolated local
   kernel HTTP endpoint working. The helper is not started and the original
   state bytes remain unchanged. A typed `--private-tailnet-unavailable` startup
   diagnostic makes the actual management router return a specific Settings
   ErrorBody; it does not invent a disabled desired state. Explicit provider
   conflicts still fail configuration validation.
2. The old `fakeNode.listenBlock` field was never read by `fakeNode.listen`.
   The replacement uses explicit entered/release/closed barriers. It waits until
   the listen operation is actually blocked, verifies refresh remains responsive,
   then proves a listener returned after needs-login or cancellation is closed
   instead of published. The corrected test passes under the race detector.
3. The API schema was regenerated for the documented unavailable-response error
   codes. The affected frontend schema/Settings tests passed (67 tests).

Additional production-only mutation evidence was collected exclusively, with
byte-for-byte restoration and green runs after each mutation:

| Mutation | Complete expected red set (actual matched exactly) |
| --- | --- |
| Propagate optional Tailnet initialization error again | `tailnet_operational_startup_failure_preserves_local_service` |
| Make pending listen synchronous | `TestPendingTLSListenDoesNotFreezeNeedsLoginStatus`, plus `/needs-login` and `/canceled` subtests |
| Remove the late-listener generation check | `TestPendingTLSListenDoesNotFreezeNeedsLoginStatus`, plus `/needs-login`; `/canceled` remains green |

Local logs/backups: `/tmp/neige-tailnet-reviewfix-mutation-09bninfn`.

Final review-fix reruns in the private Cargo target passed: 44 app/control and
configuration/packaging/startup tests, 43 server auth/pairing/ingress tests, the
complete quick Rust gates (including release compilation and OpenAPI drift), all
nine Go helper tests under `-race` with the two new blocked-listener subcases, and
67 affected frontend schema/Settings tests. The generator again exported 83 wire
bindings before emitting the updated OpenAPI document. The existing ignored
Git-source E2E fixture's required helper list was updated; that ignored test was
not counted among the executed suites.

## Fresh review logging and first-install corrections

The next independent review found that tsnet 1.102.3 writes an authorization URL
to logtail before invoking the quiet user callback. Its logger uses filch disk
buffers and an uploader. The production constructor now calls `logtail.Disable()`
before `Server.Start`. That switch stops new entries; pinned upstream source
explicitly allows previously buffered entries to drain. Release builds therefore
also use the official `ts_omit_logtail` tag, replacing both logtail and filch with
no-op implementations. `tailnet/verify-build.sh` inspects `go version -m` on the
actual binary and fails if the tag is absent. No existing logs are deleted.

`TestPrivateServerSuppressesNewLogtailEntries` first failed with two writes to
the real filch buffer and one request to an in-memory HTTP transport. It invokes
the production constructor followed by real `logtail.NewLogger`, using only a
synthetic authorization URL and a new temporary directory. It deliberately does
not rely on `tsnet.Server.Start`: upstream skips logger creation under `go test`.
With the fix, no entry reaches the buffer or upload sink. The release-tag test
`TestReleaseLogtailLeavesExistingBuffersUntouched` additionally seeds two synthetic
old buffers and verifies unchanged bytes and no upload. No account, system
daemon, network upload, or real node state is involved.

Two exclusive production-only mutations were predicted, applied, checked, and
restored from byte backups with matching SHA-256 and green reruns:

| Mutation | Complete expected red set (actual matched exactly) |
| --- | --- |
| Remove the constructor's `logtail.Disable()` invocation | `TestPrivateServerSuppressesNewLogtailEntries` in the full default Go suite |
| Remove `-tags=ts_omit_logtail` from the release build | `tailnet/build.sh` artifact tag verification rejects the newly compiled helper |

Records are in `/tmp/neige-tailnet-logging-mutation-jy_6pz3a` and
`/tmp/neige-tailnet-logging-mutation-wlulpmo1`. The first attempt at the build-tag
mutation stopped at a harness assertion before building; it restored the file
and was not counted. The corrected attempt produced the expected failure.

Final relevant checks: `go test -race -p 4 -count=1 ./...` passed all 10 top-level
tests; `go test -race -tags=ts_omit_logtail -p 4 -count=1 ./...` passed all 11.
`tailnet/build.sh /tmp/neige-tailnet-logging-artifact/neige-tailnet` built and
verified the actual static helper, and `--version` returned `neige-tailnet 0.1.0`.
The first-install runbook now declares Go 1.26.6 and builds the helper before
packaging; its build/package blocks passed shell syntax, command-order, output
path, and prerequisite checks. Shell scripts and `git diff --check` passed.
These changes do not invalidate the earlier Rust, schema, or frontend checks;
those suites were not rerun. Fresh independent reviews are still required.
