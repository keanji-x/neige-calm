# Issue 3668: named Report consumers

The final consumer stage builds on accepted core `6ae471e3f8a1e82141bf58a2b61e9ae0af57196a`.
Review tier remains L2: names cross Track discovery and isolation boundaries.
The [design](../design/unified-report-browsing.md) records the final contract.

## Ownership and delivery

`report_paths` owns the canonical codec, full user Area / sibling Track identities
and resolver. Report outlines consume those names before Report existence,
visibility, tags, query, rank or page limits. System Areas offer no Report,
block or tag references. The outline's SQL projection, legacy-body block IDs,
CRDT cached projection and `mentions::rank` ordering / eight-per-group limit
stay in their original owners. Find retains its accepted implementation.

Report/block inserts now carry `/AreaName/TrackName/report.md` and optional
`#block_id`, with the existing CommonMark fence. FE providers and the composer
send the server insert verbatim. Sent-message display recognizes the new path
shape and decodes its display label; it does not resolve, authorize, load or
reconstruct server identity. Historical sent text, old display pills, ID URIs,
backlinks, task refs and frozen migrations retain their bytes and contracts.
There is no historical browsing alias or new history service.

Deleted: `area_reports/name.rs`, `REPORTS_DIR`, and the private `file_names`
wrapper used only by the old producer. The shared codec, store, glob, snapshot,
rank and find implementations still have real callers and remain.
Runtime guides, planner/worker instructions, daily template, mail/link guidance,
consumer fixtures and current docs use named paths. Changed generated artifacts
come only from their real generators.

No new permanent scan exception, file/test inventory, CI invocation, mutable
frontend module state or application-specific identity rule is introduced.
Ordinary Area/Track additions and renames require no source list edit. Existing
suite registration selects the new tests without a separate target or pin.

## Verification

Final worker results:

| Scope | Actual result | Raw evidence |
| --- | --- | --- |
| HTTP mentions and outline/name owners | 20/20 passed | `/tmp/named-consumers-rust.log`; final baseline in mutation directory |
| FE domain/provider/composer/thread | 173/173 in four files | `/tmp/named-consumers-fe.log` |
| Stored message consumer | 2/2 | `/tmp/named-consumers-fe-message-entry.log` |
| Pending queue display and verbatim edit | 24/24 | `/tmp/named-consumers-fe-queue.log` |
| Real API generator | 193 bindings exports passed, OpenAPI emitter exit 0 | `/tmp/named-consumers-api-generator.log` |
| Registry/prompt generators | Three deliberate write-mode failures; normal-mode equality and existing budgets 6/6 passed | `/tmp/named-consumers-generators.log`; mutation directory `generators-normal.log` |
| Full-sibling naming mutation | Baseline 20/20; red 19 passed + one predicted failure; restored 20/20 | `/tmp/named-consumers-mutation/` |

The Rust owner command was:

```sh
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked -p calm-server --lib -E 'test(/^(mentions::|area_reports::)/)' --no-tests=fail --test-threads 8
```

The mutation runs add `--no-fail-fast` and execute the complete same 20-test
observation set, not just the predicted failure. The sole production mutation
names only Tracks with Reports instead of the full sibling identity set;
Area naming, visibility order, content projection and ranking stay unchanged.
The complete predicted and actual sets are identical in `predicted.txt` and
`actual.txt`; `selection.json`, `selected-names.txt`, `mutation.diff`,
`source.before`, baseline/red/restored logs and `result.json` retain the full
execution evidence. The driver is `/tmp/named-consumers-mutate.py`.
Restored source SHA-256 is
`2436f2a1da2f0da82023374d9c6a9a5e6fbbe05403508d28b80e31996de66246`;
byte comparison and the final diff confirm no mutation residue.

Both Rust and FE reproductions were red before the fix: Rust one selected
failure; FE one failure and 31 passes. Exact final FE/generator commands are
included in the task result; their raw logs remain under `/tmp/named-consumers*`.
The earlier preflight found guides at 14,007 bytes against the existing 14,000
cap; copy was shortened and all six checks then passed. No cap was raised.
The existing Planner/tool caps tighten to 8,542 / 42,725 bytes. A terminated
pre-mutation compile (exit 143) is not pass evidence; source was unchanged.
The retained preflight directories are `/tmp/named-consumers-preflight-1/` and
`/tmp/named-consumers-preflight-2/`.

Generated API changes are descriptions only; wire declarations, API operations,
registry schema/roles and DTO fields are unchanged. Registry changes are only
the mail/link description hashes. One-off structural checks are saved in
`/tmp/named-consumers-generated-audit.json`. The two generated API files are
readonly-owned; the explicitly authorized regeneration carries both required
`OWNERSHIP-CHANGE` trailers in the task's commit message, to preserve in the PR.

## Frontend qualification and final verification ownership

The qualification terminal ran in `fe/` on the exact failed candidate:

```sh
test "$(git rev-parse HEAD)" = d2df7319d58a54bd85ceeb7dbc0d8f3485a7b684 && npm test -- --maxWorkers 6
```

Attempt `962955a9f18a4018aba502aac9c643cb:qualify-frontend-full-six`
completed with exit 0: **379 passed / 1 existing skipped files (380),
6,156 passed / 1 existing skipped tests (6,157), 261.57s**. Its complete raw
output is `runs/962955a9f18a4018aba502aac9c643cb:qualify-frontend-full-six/terminal-worker.log`;
the terminal result's PTY summary was truncated, but the raw output is complete.
The test projects, selection, assertions, timeouts and runner source were not
changed. The worker cap is a local invocation resource setting, with no permanent
configuration change; CI owns its own environment. This establishes a passing
full run without proving the cause of the earlier fork failures or proving OOM.

The two earlier uncapped `finish-named-report-consumers` gates remain failed:
run 1 had 6,150 passed / 4 failed / 1 skipped tests, and run 2 had 6,148 passed /
7 failed / 1 skipped tests; each recorded an unexpected fork exit. Their first
seven steps passed (contracts 409, truth 26, focused Rust 431, admin 1 and FE
lint/build, plus text/quick). These are historical results, not green final gates.
Qualification closes the old pending runner investigation; it does not turn
those failed gates green or qualify the new upstream tree by itself.

The historical attempt
`962955a9f18a4018aba502aac9c643cb:sync-named-report-delivery` owned the replacement
frozen-target gate: text, Rust quick, full contracts, truth-files, focused replay,
admin consumer, frontend lint/build and full `npm test -- --maxWorkers 6`, in that
order. Its kernel gate result and log bind the verdict to its delivery candidate;
consult that result for final counts. Worker historical evidence does not replace
this gate. No implementation/test changes or worker broad pre-runs are needed
for this conflict-free replay. No files are to change after a passing final gate.

## Upstream replay and interdiff

Compared old patch:
`80d17b004636e2cb02f3c5c67181843fbaee0b87..d2df7319d58a54bd85ceeb7dbc0d8f3485a7b684`.
New base: `fa3a59d858f59e58c26e5945f264e2dffb2fa0dd` (`origin/main`, kernel fetched).
The new patch is that base through this attempt's frozen delivery tree.

The replay applied the complete 49-path old-base → accepted-core `6ae471e`
diff with `git apply --reject`, then the complete 48-path `6ae471e` → failed
candidate `d2df7319` consumer delta the same way. No reject or conflict occurred,
so no file required `git merge-file`; no `.rej` exists. The combined old patch
has 81 paths. Before this verification-note update, every one matched the old
candidate byte-for-byte (including new files and the deleted wrapper).
The audit is `/tmp/3668-sync-audit.json`, with one record for every path and its
consumer membership. No consumer change was already present in upstream.

The only upstream changes since the old base are nine files under
`fe/web/src/app/shell/`: `area-group.test.tsx`, `shell.module.css`,
`sidebar-group.tsx`, `sidebar.account.browser.test.tsx`,
`sidebar.hover.browser.test.tsx`, `sidebar.regions.browser.test.tsx`,
`sidebar.test.tsx`, `sidebar.tsx`, and `unified-mobile-header.browser.test.tsx`.
They have no file intersection with the Issue 3668 patch, and each is retained
byte-for-byte from the new base. There is no duplicate equivalent upstream
implementation to remove and no unrelated upstream drift in the delivered patch.

Interdiff classification: **mechanical replay plus verification documentation**;
no production, test, generator-input or generated-output behavior delta from the
old self-owned patch. The only self-owned patch difference is this note's removal
of stale pending diagnostics and addition of qualification/sync evidence.
The final scope remains all 15 design contracts: one canonical resolver/codec,
default cat, find, own/foreign/system fences, directory/date/selection behavior,
mentions and FE verbatim inserts/historical display, runtime guides, schemas and
real generated artifacts, and deletion of the old producer/name wrapper. There
are no new features, aliases, persistence or authorization changes.

Evidence reuse is input-specific: all core/consumer Rust predicates, observers,
date SQL, registry/prompt/API generator inputs and outputs, and FE mention
consumers are byte-identical to `d2df7319`; their earlier focused, mutation and
real-generator evidence remains applicable. The full-suite FE qualification and
old gates do **not** bind the newly composed upstream tree: the sidebar production
and test changes invalidate that aggregate claim. The fixed final gate rechecks
the final tree with concurrency six. No mutation or generator input was
invalidated, so none is rerun merely because the base changed. Browser/preview/
Today integration evidence still requires the Planner's final-tree follow-up.

## Reused core evidence and limits

The core foreign/file-parent predicates and their observers are unchanged.
`report_paths/mod.rs` retains SHA-256
`c0d8a4876586325582cc9abf11d1ee173703f4cca33d8b4627b42326ef6e572f`.
Preserved `/tmp/named-paths-mutations/{foreign,foreign-recovery,file-parent}`
contains the full 430-observer executions, exact predicted/actual red sets
(foreign 6, file-parent 4) and restored 430/430 green evidence. The original
backups/drivers/logs remain in place. Date SQL is unchanged. The CLI observer
module has only a migrated header; its complete behavioral body is byte-identical. The source comparison is saved in
`/tmp/named-consumers-owner-audit.json`. Core mutations are not repeated merely
for this consumer stage.

The C2b sandbox diagnostic is closed by the Planner's terminal replay on the
same accepted core: run `e1347495-5324-427f-a606-67178b9a6c92`, exact single-test
command, exit 0, 1 passed. The sandbox's duplicate `/proc` mounts rejected native
quiescence; no supervisor behavior was changed. The earlier 1024-case run is
not claimed as green or repeated.

Browser tests, real preview following `docs/development-preview.md`, Tier 1 Today,
two independent L2 reviews and CI remain with the Planner. Upstream synchronization
is completed above; these later checks must use the new delivery head. No real Codex E2E, host maintenance, formal review, manual commit,
publication or merge-readiness claim is included in this task.

## PR 3672 diagnostic delivery on fresh upstream

The current frozen-target attempt is
`962955a9f18a4018aba502aac9c643cb:sync-finish-reclaim-diagnostics`.
Its actual kernel-fetched base is
`25c77c497d205091bbda8b9d9037c343347c7919`; the local `origin/main`
reference still names `fa3a59d858f59e58c26e5945f264e2dffb2fa0dd`.
The complete `fa3a59d..0b151ce` catchup (82 paths) and explicit
`0b151ce..a3df6e7` failed diagnostic delta (one path) were applied separately
with `git apply --reject`. Both applied without rejects; no merge-file was
needed. Before this note update, all 83 paths matched `a3df6e7` byte-for-byte.
The source audit is `/tmp/962955-sync-source-audit.json`.

Compared own patches are `fa3a59d..a3df6e7` and `25c77c4..` the frozen
candidate. Their interdiff is mechanical replay plus this evidence note;
production, test and generated bytes are retained. Upstream has no path
intersection with the 83-path patch. Its #3671 identity-based terminal disposal
and exit-completion repair, #3673 first-message tests and #3646 UI/dependency
changes are retained, with no duplicate fix. UI and child-process input changes
invalidate old aggregate qualification for this new head. The current ten-step
kernel gate owns text, quick, the original full contract groups, truth-files,
focused/admin Rust, FE lint/build/full tests, planner-message and disposal replay.
Its recorded result supplies final counts; historical green runs do not.

The six serial host comparisons in
`/tmp/pr3672-reclaim-results-msf_v5tg/summary.json` actually passed: three
independent exact-case commands at `0b151ce`, then three at the full archived
`fa3a59d` baseline, one selected test per command, no retries. They did not
reproduce CI contention or establish a flake rate. Earlier preparation-blocked
slots were not red samples. PR #3513 already showed the same failure; that
still-earlier source was not built or sampled. PR #3672 run 43497980266 attempt 2
remains failed (reclaim and aggregate); these local results do not resolve it.

The diagnostic delta only runs after the original ten-second failure and bounds
all additional read-only samples to one second. It reports task status,
operation phase/blocker/lease presence, release/decision, execution settlement
presence and `ran` existence; it neither dumps secrets nor drives recovery.
The original loop, fence lifetime, failure expectation and safety assertions
remain intact. No production reclaim behavior or test deadline was changed.
The failed diagnostic addition still needs two independent mechanical-delta
reviews; any behavioral interdiff instead requires both L2 channels.

Cargo metadata for this replay identifies the current checkout's manifests and
library `src_path`s for calm-truth/server/types and the existing shared target.
Current calm-truth unconditionally exports `TrackFsNodeKind` and implements
`TrackFsView::node_kind`; the old r2 unresolved-import/method errors are not
explained by a missing current API. Source/feature/verbose artifact evidence is
retained under `/tmp/962955-cargo-metadata.json` and
`/tmp/962955-truth-source-vv.log`. No compatibility fallback, manifest workaround,
cache deletion or alternate target is part of this delivery.

Unchanged owner predicates and their mutation observers retain their earlier
focused/mutation evidence; unchanged schema/generator inputs and outputs retain
real-generator evidence. Base movement alone does not require another mutation
or generator run. New-head browser/Today/real preview, required reviews and CI
remain Planner follow-ups. No CI rerun, publication or real Codex E2E was run.
