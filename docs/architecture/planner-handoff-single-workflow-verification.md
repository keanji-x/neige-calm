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

## Legacy evidence / settlement correction

This slice remains L2 (persistence and exact-claim isolation). Its baseline is
629257bb41d016c87f03634219d6cb2949970df9; it does not replace the converged normal
single-owner workflow or synchronize main. The two source reviews are Track runs
`handoff-single-owner-review-a` (completed event 428099) and
`handoff-single-owner-review-b` (completed event 428025).

The allowed matrix is in [the design](planner-handoff-single-workflow.md#legacy-evidence-and-settlement-correction-l2).
`handoff_legacy_evidence` is the sole V1 interpretation boundary for coordinator
query/adoption and activation query/adoption. Required nullable V1 wire members
must be present; null is not the same as an omitted transport field. Its typed
conversion checks business stage, original scope, exact receipts/releases, recorded
cutover result/hash, preparation or immutable native proof, transport and current
binding. Historical Activated validates its immutable evidence before ignoring
later mutable ownership. Sending/Unknown cannot become absence or Prepared; proven
resources cannot become Prepared when a transport field is damaged.

A legacy Finished receipt is not a settlement certificate. Query is read-only and
returns NeedsRecovery until explicit original-generation adoption. The adopter
checks the old lease/generation and original SQL claim evidence under IMMEDIATE,
then the existing workflow serial Resolve/typed release owner reconciles both
original requests. Its existing terminal transaction owns reservation release,
including ignored-write detection. Neither a cleared attempted flag nor a frozen
operation revokes a pause already beyond its intent seam.

0173 therefore rejects exact old Finished claims and exact adopted coordinator
claims. Adoption fencing starts before Resolve: a command cannot land between
serial absence and terminal persistence. Already-landed identical paused claims
remain writable for health/release reconciliation. A different new claim is not
fenced. Two partial expression indexes cover only legacy coordinator source/target
identities; each eligible pause snapshot write uses at most two indexed exact-claim
lookups plus indexed adoption lookup, rather than introducing a workflow-history
scan. The existing 0172 fences and their previously documented costs are unchanged.
0172 and every earlier migration remain byte-frozen.

Actual first red: `/tmp/handoff-legacy-first-red.log`, complete specified focused
selection, 265 tests = 263 passed + these two failures:

- `handoff_coordinator_legacy_proven_transport_damage_rejects_before_adoption_or_mint`:
  actual ResourcesCheckpointed with missing transport thread was accepted for adoption.
- `handoff_coordinator_legacy_finished_requires_explicit_original_claim_settlement`:
  the old reader returned terminal instead of NeedsRecovery.

Fixture provenance and limits are explicit. No old executable was run. Native
proof fixtures invoke the real start adapter against the daemon stub and a small
frozen bind/finalization seam from f1f9e5b7a's production activation SQL/interact
path. Only its removed facts wrapper, surrounding test transaction and runner
setup are adapted. The frozen 0170 pending transition trigger is read from its
unchanged source and installed/bound/restored in one transaction. The real generic
leased checkpoint, token binding, snapshot writers, transport owner and harness
installation run; no successful native proof is authored by a test. Corrupt
transport/state members are injected only after that real proof exists.

The Finished no-effect wire fixture represents the old checkpoint after a real
serial absence barrier; it is not claimed as execution of an old compensator.
The separate post-intent interleaving uses the public production begin/pause seam,
changes only its persisted representation to V1, expires its lease, and delivers
that still-live original command while the adopted owner is stopped after Resolve
and before terminal commit. This exercises actual commands, storage and fences,
not a handwritten pause or compensation algorithm. The pre-upgrade already-landed
case executes the real pause with 0173 temporarily absent, reinstalls it, and then
uses explicit original-generation recovery. Historical activation with active=1
comes from real install/finalization and the old unchecked UPDATE under a real
RAISE(IGNORE) trigger; recovery includes SQLite file reopen and production Track
delete. Faulted and recovered/unconfirmed typed releases are exercised through
real serial owners before legacy adoption. No real provider, Tier 2, UI/browser,
remote CI, full historical binary upgrade or broad workspace suite is claimed.

Intermediate focused runs `green-1` through `green-7` were not green. They exposed
an adopted-reader replay lookup error and fixture issues (direct Track SQL delete,
nontransactional trigger replacement, and a test task that asserted a particular
old-driver return instead of the durable fence). They are not delivery evidence. The implementation fixes
the replay lookup; fixtures use production deletion, atomic schema restoration and
byte-for-byte equality of the parked old journal after the late command. The post-intent fixture also applies the
actual 0172 parking shape, so the generic runner does not try to claim its frozen
old journal after the adopted owner completes. The original command remains live
across that parking; it still reaches the actual pause owner.

Deleted mechanisms: optional-string resource-stage guessing, the legacy Finished
query/retry success shortcut, and the separate cutover read-back transaction wrapper. Raw V1 fields now live only inside the decoder; its result carries
required typed progress. No adapter registration, executor, journal, repeated
persistent field, normal operation or domain checkpoint is added. A fixture-only
pause point observes the existing settlement boundary. Original journals, audit,
proof, key mapping, reservation, generations and full-row recovery remain because
they own historical identity, exclusion, ABA, native intent and queue/floor/ACK
integrity. New responsibility files remain below 800 lines.

Permanent maintenance: ordinary new tests change their module only; ordinary new
providers continue through the existing profile/start/recovery owner. A new V1
supported shape changes this decoder and its boundary tests. 0173 adds its one
filename to the existing schema inventory; future migrations retain that existing
two-file contract. No new CI pass, test-name pin or provider list is introduced.
The task's six-step kernel gate owns text, quick, contracts, focused replay, truth
authorization and frontend consumer checks; the worker runs only the complete
specified focused selection and necessary formatting/lint fixes.

Final worker focused run: `/tmp/handoff-legacy-green-9.log`, 268/268 passed.
Two explicit single-factor production mutations each ran the entire same selection:

| Mutation | Complete predicted and actual red set | Restore |
| --- | --- | --- |
| State/proof thread equality changed to nonempty | Only `handoff_coordinator_legacy_proven_transport_damage_rejects_before_adoption_or_mint` (267 passed, 1 failed, exit 100) | 268/268, exit 0 |
| Only 0173 late-claim guard disabled with `WHEN 0 AND` | Only `handoff_coordinator_legacy_finished_requires_explicit_original_claim_settlement` and `handoff_coordinator_legacy_source_intent_expired_driver_cannot_relock_after_adoption` (266 passed, 2 failed, exit 100) | 268/268, exit 0 |

The complete pre-write predictions, proposed diffs, original/mutated file hashes,
all tracked/untracked source hashes, before/applied/restored diffs and exact red-set
comparison are `/tmp/handoff-legacy-mutation-{proof,claim}.json`, `.proposal.diff`,
`.before.diff`, `.applied.diff`, `.restored.diff`, `-red.log` and `-restored.log`.
Both restores (including after restored green) exactly matched tree SHA256
`13f1fb69de6acde6e15928275fce4f88a2aa6472ca34b652ebacfca1c12a3a5a`
and full diff SHA256
`3ffd37a7b6d08f5541ff1b62d7e27ed2c4a205a675c1bc0391c61bffdb1e4b58`.
The prior normal live/absorbing/forward mutation target files remain byte-identical
to their admitted records (`/tmp/handoff-legacy-prior-invariant-bytes.json`);
those records are retained only for those unchanged invariants. The 172 existing
migration byte comparisons are `/tmp/handoff-legacy-frozen-migrations.json`.

After these restores, only this verification text and a mechanical test-only
producer module rename were added before the gate: the frozen producer now lives
in `handoff_legacy_fixtures_tests.rs`, declared under `cfg(test)`. Its bytes are
identical to the tested producer. Call sites and the test module declaration only
change that module identifier. This follows the existing production-writer scan's
`*_tests.rs` convention; no production writer inventory entry or exemption is added.
Both production mutation target files remain byte-identical to the restored hashes.
The kernel focused replay verifies that final registration and frozen target. Kernel
results belong in the task report/log, not an edit after the passed gate.

Gate run 1 stopped at text gates. Its mechanical correction names the private
transport member `session_id` with the unchanged V1 wire key in one serde rename.
That necessary frozen-wire reference raises the existing terminology counter by
one (1237 to 1238), with an enumerated rationale in the gate header and the actual
baseline generator run. No pattern, excluded path, test roster or extra CI pass
changes. Seven SQL literals now use compile-time concatenation; their resulting
SQL bytes are identical (`/tmp/handoff-legacy-text-fix-equivalence.json`). The
strict thread comparison and 0173 guard are unchanged. These later source-format
and private-name changes are distinct from the exact restore hashes recorded
above; the final kernel replay verifies them. The existing ratchet still runs once
in each declared text-gate invocation, and ordinary new tests require no entry.

The subsequent quick gate diagnosed only `unnecessary_mut_passed` in the frozen
producer's finalization seam. Removing that local variable/reference mutability
changes no production code, SQL bytes or checkpoint arguments. Runs 2 and 3 both
record that same lint failure on the same tree; the gate join request started the
third run after the preceding run had ended. The final gate verifies this mechanical
fixture correction. No historical failed gate is reported as green.

Gate run 4 passed text/quick/OpenAPI and stopped at the deferred-transaction source
contract (plugin contracts 195/195 and integration contracts 13/13 passed; server
contracts were 177/178). Its empty read-only allowlist explicitly requires that a
transaction cannot park holding read locks, so these multi-statement async evidence
reads do not qualify. Both legacy query snapshots now reuse `begin_immediate_tx`,
the repository's existing deadlock-safe transaction owner. They execute SELECTs
only, roll back, and release before querying a mapped owner on another connection;
they acquire no operation lease, write no row, adopt nothing and call no provider.
This briefly serializes legacy evidence reads with writers, preserving a consistent
state/proof/binding/reservation snapshot without a shared-cache read-lock cycle.
Normal workflow queries retain their existing entry point. No deferred-read
allowlist entries or new permanent pins are added. The removed mechanism is the
separate cutover read-back wrapper, not the existing transaction primitive. The
final kernel focused replay rechecks pure reads and every integrated recovery path.

## Compensation revocation correction

This slice continues rejected f0086f2ed63329d20148f393d8f0577098c6757d,
without a mainline sync. It remains L2: the original claim's execution authority
and persistent settlement boundary change. Original source reviews are
`handoff-legacy-settlement-review-a` (completed event 431065, blocking) and
`handoff-legacy-settlement-review-b` (completed event 431114, pass). The decision
uses the production call path and a new failing interleaving, not review votes.

The unified [revocation and settlement matrix](planner-handoff-single-workflow.md#irreversible-compensation-boundary-l2)
uses existing Compensating progress as permanent revocation. Its existing
lease/output CAS transaction commits before Resolve; an error or ignored UPDATE
cannot reach the absence branch. The new 0174 migration makes this state monotone,
freezes the original request bindings after revocation, and combines 0172's
terminal-only V2 fence and 0173's separate legacy fence into one creation guard.
Existing exact paused claims retain fault/health and typed-release authority.
The final full-snapshot UPDATE consumes this fact for both source and target.
No send-time memory lease check, driver mutex, TTL, new executor or new field is
used as the commit fence.

### Actual first red

Both runs used the complete specified focused command (not a Python SQL model).
`/tmp/handoff-revocation-first-red.log` selected 270 tests: the original 268 passed,
and only these two newly added tests failed with
`late original pause committed after absence`:

- `handoff_coordinator_live_source_intent_cannot_land_after_serial_absence`
- `handoff_coordinator_live_target_intent_cannot_land_after_serial_absence`

That first fixture used the existing unstarted harness's production command handler
and serial admission lock. To exclude a fixture-only explanation, both harnesses
were then changed to actual running harness loops and the entire selection rerun
before the fix. `/tmp/handoff-revocation-running-first-red.log` again selected
270 = 268 passed + exactly the same two failures, for the same assertion.

Both runtimes use the same registry and SQLite file; the second runtime actually
reopens the file and invokes ordinary `drive()` / `claim_drive`. The only lease
fixture is `lease_until_ms=0`, the same deadline seam used by repository claim
tests. Neither lease owner nor operation phase/output is manually claimed or
changed. Each test verifies the production claim changed the owner, the original
begin future is still live, Intent has reached Compensating/Untouched after real
serial Unknown, and reservation remains active while settlement is stopped.
It then releases that same original future's send, waits for its actual pause SQL,
and lets the second runtime commit Finished. Before the fix this yielded a paused
original session with active=0. No original future is aborted, no successful receipt
is authored, and no business progress is injected in these V2 reproductions.

### Maintenance and limits

Removed mechanisms: the V2 terminal-only creation fence and the parallel legacy
creation fence; their two legacy-only indexes are replaced by two partial indexes
covering the two domain protocol kinds. Removed assumption: a past serial absence
alone grants future terminalization while an unsent original command remains live.
No registration, repeated persistent field or normal checkpoint is added. Retain
one journal, thin reservation, original scope/claims, ABA generations, immutable
resource/adoption proof and full-row snapshot/CAS for their existing independent
identity, exclusion, Unknown, health and queue/floor/ACK responsibilities.

An initial OR query plan scanned a partial index. The implemented UNION ALL
candidate projection gives two `SEARCH ... (<expr>=?)` exact index plans and a
legacy-adoption primary-key lookup (`/tmp/handoff-revocation-index-plan.txt`). It
stores no new projection or queue. Ordinary later tests/providers require no new
list; a migration still updates its SQL file and the existing schema inventory.
0173 and all earlier migration bytes remain frozen. The kernel owns all six gate
steps; the worker only executes the full focused selection and necessary formatter
or lint fixes. The focused replay is the one intended duplicate check.

No actual old binary, real provider inference, Tier 2, UI/browser, remote CI or broad
workspace suite is claimed. The legacy wire-conversion fixtures remain explicit
protocol-boundary fixtures, rather than historical binary execution. Native calls
use the established daemon stub. New fixture seams only stop the real compensation
entry and committed revocation checkpoints; they do not synthesize execution state.

The additional matrix tests use both source and target. Controlled final-write
orders stop the real original pause after staging but before its final snapshot
transaction, and stop the second leased runtime before and after revocation.
Pause-first preserves the actual receipt and reconciles HealthyReleased;
revocation-first rejects creation. A separate SQLite-file test releases both
writers behind a third IMMEDIATE transaction and accepts either winner, asserting
real Finished never leaves either session paused. The legacy post-intent/adoption
fixture now exercises target as well as source, preserving its original assertions
and byte-frozen old journal. Its target's source receipt comes from the real
previous source pause, not a constructed proof.

Ignored-revocation tests inject an actual RAISE(IGNORE) on the Compensating UPDATE,
verify persisted Intent and active exclusion remain, release the still-live old
send, then repair storage, reopen the file and explicitly retry the original
operation/generation to release its actual receipt. The existing checkpoint ABORT
regression remains. Another test loses only the compensating future after the
real revocation commit but before Resolve, reopens the file, and verifies the
uncancelled original future still cannot create its claim. It expires only the
new dead owner's lease to drive actual recovery. This models lost-driver/answer
recovery after a known commit, not an injected indeterminate SQLite COMMIT I/O.
The latter remains a source-level fail-closed argument: any checkpoint/commit error
returns before Resolve; only a later successful leased checkpoint can settle.

Fault and recovered-health tests change the existing exact paused claim *after*
revocation commits. Real notification and full-row recovery owners can still
persist FaultedPaused/RecoveryHealthUnconfirmed and typed release on either side;
nonhealthy release retains NeedsRecovery and active exclusion. Ordinary fresh
claims remain available. The original normal single-owner, Activated absorbing,
Unknown-no-remint, full snapshot, rewind, ACK/floor and worker/approval-history
coverage stays in the same complete focused selection.

### Final focused and single-factor mutation evidence

Worker green runs are `/tmp/handoff-revocation-green-1.log` (273/273), then
`/tmp/handoff-revocation-green-2.log` (275/275 after the concurrent writers,
post-revocation owner loss and legacy target extensions). All use the complete
specified selection, including both integration binaries. They preserve the old
268 observers; they are not offered as proof without the new failing interleaving.

The final production mutation changes only 0174's workflow creation predicate
from `compensating/finished/activated` to `finished/activated`. It leaves the
monotone transition, original binding protection, legacy and terminal guards
intact. Tests and fixtures are unchanged. Before writing, the complete predicted
red set was recorded by full test name:

- `harness::run_loop::handoff_coordinator_revocation_tests::handoff_coordinator_live_source_intent_cannot_land_after_serial_absence`
- `harness::run_loop::handoff_coordinator_revocation_tests::handoff_coordinator_live_target_intent_cannot_land_after_serial_absence`
- `harness::run_loop::handoff_coordinator_revocation_tests::handoff_coordinator_final_pause_and_revocation_transactions_serialize_both_orders`
- `harness::run_loop::handoff_coordinator_revocation_tests::handoff_coordinator_revocation_survives_owner_loss_and_file_reopen`

`/tmp/handoff-revocation-mutation-red.log` actually ran all 275: 271 passed and
exactly those four failed (exit 100). The comparison rejects missing or additional
failures. `/tmp/handoff-revocation-mutation-restored.log` ran the entire selection
after byte restoration: 275/275 passed (exit 0). Mutation ran with one exclusive
writer and no concurrent review. The 0174 source SHA256 before and after restore
is `03289033eb96feed477fae885e4111d6b94a81575749f925d9de33f0fb42a411`;
the mutant is `053a1ea1c3527589926c8a5ae70b8b0fe3a0d254dcd8b3289b759efa235e12d2`.
Complete tracked/untracked file maps, pre-write prediction and proposal, applied
single-file check, actual red-set comparison and post-green restoration checks
are `/tmp/handoff-revocation-mutation.json`, `.proposal.diff`, `.before.diff`,
`.applied.diff` and `.restored.diff`. Before, restored and post-green tree hashes
all equal `523573faf6d4a6320b1579c8a6393d1a3cf33645d68b19056d7bf714f06016a4`;
the full diff hash in all three is
`d193b06d0484f6e842121bc8474b9c0e6eb4af8210e8e2c13eabd3096b54318a`.
Only this final verification text is appended after that exact restoration check;
production and test bytes stay identical for the kernel's frozen-target replay.
The complete 173-file pre-0174 migration equality/hash record is
`/tmp/handoff-revocation-frozen-migrations.json`.

No API schema, generated binding input, frontend file or visible UI changed in
this slice, so no generator, ownership-change trailer or browser preview is
required for its delta. Earlier whole-Track frontend changes retain their own
ownership history; this statement does not erase them. Kernel results belong in
the task report and exact gate log, rather than an edit after its passed run.
