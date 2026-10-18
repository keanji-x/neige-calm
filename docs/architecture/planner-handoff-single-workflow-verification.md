# Single workflow implementation verification

L2: the single owner crosses authorization, durable identity and resource boundaries.
Implementation follows [the accepted design](planner-handoff-single-workflow.md).
The six-step final kernel verdict is attached to the task report and its gate log;
this document records worker evidence and does not predeclare that verdict. Planner
owns the two fresh independent L2 review channels.

## Original real red evidence

The accepted base is `c4d63190445cb8f618fcfdcbd866cbc009099a14`. Overall review A
(completed 422185) found the postcommit child Stuck/reservation dead end; review B
(completed 422132) found Reserved approval reacquisition and nonabsorbing Activated.
Those findings are addressed by one operation, not by retaining children behind a facade.

The original genuine first-red run is `/tmp/handoff-workflow-original-red-2.log`,
nextest ID `b73f0617-b980-49c4-8ce5-b15b8eabe56b`: **257 selected, 254 passed,
three failed** (exit 100). The original regression source SHA-256 was
`f79e713b14ddc9fe69a308160f49829ad4b93e922a9f8e8e7852de5f9e146a24`.
The earlier run's two fixture-projection failures are not counted as defect evidence.
The old red tree was not synchronized or replayed in this implementation attempt.

| Genuine original regression | Corrected production boundary |
| --- | --- |
| Reserved crash without cancel, same-session busy/idle and file reopen | Recovery compensates or finishes without a new pause; a delayed exact original claim cannot recreate a settled pause. |
| Identity committed, prepare storage ABORT, repair/reopen/original-generation retry | The same operation remains Committed/Unprepared and forwards after repair. There is no child operation. |
| Activated plus inactive reservation committed, answer lost, hold and reopen | Query/retry/boot retain historical Activated without native mint or reinstall. |

These tests still use actual typed begin, SQLite, the runner and native stub; no
successful pause/cutover/activation receipt is seeded. Obsolete child assertions
now require the original operation ID, its exact resource stage and one operation
with no child. Admission input/claim identity assertions remain.

## Focused command and results

The exact full task-provided command/filter is retained in task acceptance, recorded invocations and kernel step 4. The same library plus two integration targets, nonempty selection and concurrency caps select 263 tests; no filter was narrowed.


`/tmp/handoff-workflow-implementation-13.log`: **263/263 passed**, 2066 skipped.
Run 11 also passed 263/263 before the shared start fixture's confirmation barrier.
Run 10 also passed 263/263 before the final concurrent-receipt test correction.
The final restored mutation runs below each replay this entire 263-test selection.
A prior intermediate run (`implementation-8.log`) passed 262/262 before the last
legacy runner-tail case was added. Intermediate compile/test logs are retained;
they are not independent final acceptance runs.

The measurement run (`implementation-9-measurement.log`, with only
`--success-output final` added) selected 263: 262 passed and one existing viewer
projection-error test failed because the new late-pause trigger called
`json_extract` on deliberately malformed non-handoff JSON. The trigger now checks
only valid typed pause facts; it does not reinterpret malformed snapshots as
healthy or grant authority. The original viewer test is unchanged and run 10 is
green. No scanner, assertion or golden was relaxed to hide this failure.

The normal pipeline's real SQLite audit in that measurement run recorded **10**
domain output changes: `reserved` → `pausing:intent/untouched` → `pausing:paused/untouched` → `pausing:paused/intent` → `pausing:paused/paused` → `committed:unprepared` → `committed:prepared` → `committed:native_intent` → `committed:proven` → `activated`. This is one initial output plus nine leased
checkpoints, against the former 14 changes across three operation rows. Generic
runner phase writes are excluded from that domain count. The temporary audit table,
trigger and print were removed, restoring the test file exactly to SHA-256
`5a514763722d595a6d279717a122d809728ff2c0827da68ff231d38e82cf62f6`.
No permanent exact-count list or extra CI pass was added.

Package lint iteration ran:

```sh
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 \
  cargo clippy --locked -p calm-server --all-targets \
  --features calm-server/codex-e2e -- -D warnings
```

`/tmp/handoff-workflow-clippy-3.log` passed after fixing actual lint findings.
Final-head fmt/clippy, text and contract checks are owned by the frozen kernel gate.

Kernel run 1 stopped at text checks: new identifiers used retiring template vocabulary,
SQL literals needed formatting, and a rendering-hint echo added retiring terminology.
New identifiers now name handoff state/resources and the recovery owner; all 15 SQL
statements were formatted without token changes. The activation SQL proves its target
from immutable domain input instead of repeating the rendering hint; existing Rust
output/installation checks retain the declared ABI contract. Scanner rules and
baselines are unchanged (long literals remain 148). The same full focused selection
and three mutations were rerun after this convergence. Pre-text-gate mutation logs
are retained separately and are not used as final evidence.

Kernel run 2 passed the other text checks and caught one mechanically renamed
historical migration filename in the head inventory. The frozen 0079 name was
restored; no allowlist entry or scan rule changed. This is an inventory-only
correction after the final source mutations; the kernel checks that frozen target.

## Critical single-factor production mutations

An initial absorbing run was invalid: its actual red set had the intended terminal
case plus a concurrent fixture that incorrectly required an immediate same-key
`Running` receipt to be Activated. It is preserved as
`/tmp/handoff-workflow-mutation-absorbing-invalid-1.*`; restoration passed 263/263.
The concurrent test now uses the real native-send barrier, verifies immediate
Running on the original owner, then exact terminal/replay owner IDs, one native
resource and no reinstall. Compatibility receipts reuse the generic pure
snapshot-to-receipt projection from one captured row. Generic observation policy
is unchanged. A second absorbing run matched its one-red prediction but its first
restoration exposed the existing interrupt fixture's pre-confirmation capture:
TurnStarted correctly consumed issued_turn_id between the before-image and stop.
That run is `/tmp/handoff-workflow-mutation-absorbing-restoration-invalid-2.*`
and is not claimed green. The shared start fixture now waits for the actual
TurnStarted confirmation before capturing the stable owner; every original
staging/snapshot/deadline/shutdown/native-effect assertion remains.
All three mutations below were rerun on that same final tree;
the earlier live run is retained separately and not used as final evidence.

Each prediction, exact mutation diff, source hashes and complete worktree diff/hash
were written before mutation. Tests were unchanged; one production factor changed
at a time. Each red run and restored run selected all 263 tests. Complete actual
red sets matched predictions, then restoration returned 263/263 green with identical
file and worktree hashes. No concurrent writer/reviewer observed mutation work.

- **live**: 2 predicted/actual failures, exit 100; restored exit 0.
  `calm-server harness::run_loop::handoff_coordinator_fault_tests::handoff_coordinator_legacy_reserved_or_paused_adoption_preserves_claim_and_fences_old_driver`
  `calm-server harness::run_loop::handoff_coordinator_regression_tests::handoff_coordinator_reserved_restart_without_cancel_never_reacquires_live_approval`
  Metadata/diff: `/tmp/handoff-workflow-mutation-live.json` and `.diff`; logs: `-red.log` and `-restored.log`.
  Original/restored file SHA-256: `d418be80c34704947309b24028caf64dafb019874aa0ec0605b17f154ec9fc39`.

- **absorbing**: 1 predicted/actual failures, exit 100; restored exit 0.
  `calm-server harness::run_loop::handoff_coordinator_fault_tests::handoff_coordinator_activated_runner_tail_stuck_and_successor_handoff_preserve_history`
  Metadata/diff: `/tmp/handoff-workflow-mutation-absorbing.json` and `.diff`; logs: `-red.log` and `-restored.log`.
  Original/restored file SHA-256: `d418be80c34704947309b24028caf64dafb019874aa0ec0605b17f154ec9fc39`.

- **forward**: 1 predicted/actual failures, exit 100; restored exit 0.
  `calm-server harness::run_loop::handoff_coordinator_fault_tests::handoff_coordinator_each_resource_storage_stage_reopens_and_forwards_or_retains_unknown`
  Metadata/diff: `/tmp/handoff-workflow-mutation-forward.json` and `.diff`; logs: `-red.log` and `-restored.log`.
  Original/restored file SHA-256: `8bdcfde5b593d39af3057c9ca83a20058b7f5990e9041f423b7a58f674f170bc`.

The live mutation defaults recovery into live eligibility (two failures: original
Reserved recovery and recovery-only legacy Reserved adoption). The absorbing mutation
makes technical Failed/Stuck take priority over business output (one runner-tail /
successor-handoff failure). The forward mutation incorrectly requires live eligibility
for Prepared continuation (one storage-stage matrix failure). Shared before/restored
tree SHA-256: `c5bddfed8bfb773d4edc8978f9ea860944f7383b5fbe743ef4339f4a250be01f`.

## Coverage and retained boundaries

The storage matrix enters through actual begin and reopens a SQLite file after each
fault: prepare, before sending intent, bind, install, terminal checkpoint and ignored
reservation release. Safe stages reach Activated on the same key/generation. A
returned native resource whose bind failed remains Unknown and is never minted again.
Stale generations cannot drive effects. The terminal transaction proves reservation
release instead of accepting an ignored SQL update.

Separate actual regressions cover committed identity answer drop, Activated followed
by runner-tail Stuck, completion of the real first provider turn, a second legitimate
handoff, and historical retry/query of the first owner. Native counts, consumed claims,
current owner identity and reservation close/delete behavior stay asserted.

Legacy tests preserve original input/output bytes and exact claims, cover Reserved
and real Paused recovery adoption, persisted native intent without success fabrication,
and a real committed cutover with an old runner Stuck. Old drivers are SQL-fenced;
standalone cutover tests exercise the retained SQL helper, and resource/recovery tests
use the bounded adopted owner. Legacy native intent uses the real briefing/snapshot
preparation owner and records no fabricated native resource or successful result.
Historical cutover receipt reads and the explicit version-one decoder remain; the
new owner references immutable legacy audit/proof and copies complete proof atomically.
No old three-operation executor is registered or driven after upgrade/adoption.
These are local protocol/native-stub checks, not a replay of an old provider binary
or proof of every historical provider deployment/file.

The original scope/role/credential revocation, task/worker/approval identity,
notification prefix/floor, pending queue below floor, ACK/CAS, full-row snapshot,
Resumed, interrupt and rewind regressions remain in the same focused selection.
Nonhealthy typed releases remain explicit NeedsRecovery with exclusion retained;
they do not become Healthy or permit deletion merely because a claim was released.
Native Unknown requires evidence-based recovery and never retries a native request.

Removed normal mechanisms: independent cutover/activation drivers and registrations,
activation submit/enqueue/child retry key, claimed-tx-only composition, reverse
coordinator JSON reservation release, duplicate stored plan/result/request hash,
PlanRecorded/CommitIntent and empty Committed rewrites. Both production registries
now have one handoff adapter; declared non-task kinds drop from 13 to 11. Retained
thin reservation, cancellation, binding generations, immutable resource/retirement
proof and full snapshot recovery each have distinct authority or ABA/health duties.
Migration 0172 is new; migrations through 0171 are byte-frozen. The existing head
schema inventory gains only the new migration filename.

No public API schema or generated binding input changed. No frontend file or visible
UI changed, and no generator output, browser preview, real provider inference,
Tier 2 or remote CI result is claimed. Codex native counts come from the existing
stub; other-provider boot/shared recovery cases in the focused selection do not
establish real handoff inference for those providers. Kernel step 6 validates the
API consumers on the final frozen head. A generator/ownership trailer is required
if that gate discovers actual generated input drift; no such drift is hidden here.

## Final production source hashes

- `crates/calm-server/src/operation/handoff_coordinator.rs`: `d418be80c34704947309b24028caf64dafb019874aa0ec0605b17f154ec9fc39`.
- `crates/calm-server/src/operation/handoff_coordinator_adapter.rs`: `a2a32a0c9281b424b55a87e6cf7c65cee0939cc5f333ecc82d0225c286838fa2`.
- `crates/calm-server/src/operation/handoff_state.rs`: `f16d1be04bec3f7b1d7ab571a5b05763cf655f81991e519999696857f238c609`.
- `crates/calm-server/src/operation/handoff_resources.rs`: `8bdcfde5b593d39af3057c9ca83a20058b7f5990e9041f423b7a58f674f170bc`.
- `crates/calm-server/src/operation/handoff_legacy.rs`: `6d3363eabf219b9ee73519a3c594ff910b3f0da4509268b72bc56f6d89c98898`.
- `crates/calm-server/src/operation/interaction_retry.rs`: `ae4ec8052f0873e08be0f889098b6f60f55fcdec93c219769bd80ef94b4ab45f`.
- `crates/calm-truth/migrations/0172_planner_handoff_owner.sql`: `eb5a33f995b76f35f858c04f58e093c72eedfac96737be2f1b92a3ebd41118f3`.
- `crates/calm-server/src/harness/run_loop/handoff_coordinator_regression_tests.rs`: `ba200e5bd298ce0ac5cc182ad94068be02bad7c1aab26d12eed3448f7aba68c0`.
