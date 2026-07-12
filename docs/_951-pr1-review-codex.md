# PR1 review — issue #951 Today Launchpad backend foundation

## VERDICT

**fix-then-ship**

The overall Slice A/D shape matches the locked design, and the singleton is protected by the intended global partial unique index. One correctness bug makes the nominally idempotent ensure endpoint destructive, however. I found **0 BLOCKER, 1 MAJOR, and 1 MINOR** findings.

## Findings

### MAJOR — Every ensure erases the existing concierge transcript

**Evidence:** `crates/calm-server/src/routes/today.rs:67-68,93-111,247,269,287-288`

`today_launchpad_ensure_tx` distinguishes an existing marked launchpad with `created = false`, but its existing-spec-card branch unconditionally deletes every `harness_items` row and replaces the card payload with a fresh empty harness snapshot. Both the initial attempt and the conflict retry call this same function. The `created` flag only reaches the later harness-start request, where it disables `reset_harness_items` and `force_new_thread`; by then the transaction has already erased the durable transcript and reset the payload.

This violates the endpoint's get-or-create/idempotency contract and Slice A's stated policy to reset **only an adopted legacy wave's spec thread**. A page refresh or any repeated bootstrap call can silently destroy the user's Today conversation. Concurrent callers make the effect especially surprising: the winner creates/adopts, while a later serialized caller selects the winner and clears what may already have been written.

**Concrete fix:** track a separate `adopted_legacy` boolean (do not overload `created`). Run `DELETE FROM harness_items` and reset the existing spec payload only when `adopted_legacy` is true. Use that same flag for `force_new_thread`/`reset_harness_items`; a freshly inserted wave/card already has a fresh payload, while an ordinary existing launchpad must remain untouched. Add an idempotency regression covering two sequential ensures with a persisted harness item between them, plus the concurrent ensure case.

### MINOR — The race recovery catches every database error and obscures retry failures

**Evidence:** `crates/calm-server/src/routes/today.rs:253-275` (the same broad pattern also appears for system-cove creation at `:216-223`)

The comment says recovery is for a partial-unique-index loser, but the match retries on any `CalmError::Db(_)`. That includes I/O, corruption, busy/locking, schema, and unrelated constraint errors. Worse, `.map_err(|_| e)` discards the retry's actual error and reports the first one. The unique index still makes successful concurrent ensures singleton-safe, so this is not itself a duplicate-creation bug; it is overly broad recovery and misleading diagnostics that can conceal a real database fault.

**Concrete fix:** inspect the underlying SQLite database error and retry only the expected unique-constraint violation for `idx_waves_one_launchpad` (and the corresponding system-cove unique constraint). Propagate all other errors immediately, and if the retry fails, return the retry error rather than replacing it with the first error. An `INSERT ... ON CONFLICT ...`/select pattern is also reasonable if it preserves the deterministic legacy-adoption transaction.

## Focus-area conclusions (no finding)

- **Singleton semantics:** migration `0063` implements the design's one global launchpad, not one per cove. SQLite serialization plus the partial unique index prevents two marked winners; the retry issue above concerns error classification, not uniqueness.
- **Legacy adoption:** selection is deterministic (`created_at,id`) and restricted to the system cove, null purpose, exact `Today` title. It preserves the first terminal card that has a linked terminal and creates missing spec/report/terminal cards. Clearing workflow binding and resetting the adopted spec transcript are explicitly required. No separate wrong-wave/data-loss defect is evident beyond the unconditional-reset bug above.
- **Idle boot:** `goal: None` is correct. Reset/force-new should apply to adoption, but not every newly minted row merely because `created` currently combines mint and adoption.
- **Marker integrity:** `NewWave` has `deny_unknown_fields` and no `purpose` member (`crates/calm-truth/src/model.rs:91-125`); normal `wave_create_tx` writes `purpose = NULL`. The only production assignment to `launchpad` in this diff is the dedicated ensure transaction.
- **cwd:** `app.daemon.data_dir` is the terminal runtime directory (`<data_dir>/terminals`), so its parent yields the stable app data directory and `launchpad` is an intended sibling of `terminals`, not outside app data. Creation, canonicalization, and `is_dir` validation happen before harness start. The fallback for a parentless relative path is safe, though production configuration is resolved to an app-data path.
- **Terminal role:** `CardRole::Worker` matches ordinary terminal-card creation/composite conventions; it is not the concierge identity (the spec card is).
- **Authorization:** the route is mounted under `protected_router` and requires the `Actor` extractor. Using `Kernel` for server-owned bootstrap rows is appropriate; ignoring which authenticated caller triggered idempotent global bootstrap does not grant cross-cove mutation through request fields because there are none.
- **Git probe:** it invokes `git` directly with argument separation (no shell), clears the environment and restores only a fixed PATH/HOME plus restrictive Git/locale variables, disables prompting and optional locks, caps retained output while concurrently draining the pipe, kills and waits on timeout, rejects oversized/non-UTF-8 output, and never logs the remote. Normalization strips authority/userinfo and accepts only exactly two conservative ASCII path components. I found no credential leak or command injection. Unsupported forge path layouts become unresolved rather than being guessed.
- **Migration `0062`:** nullable identity and timestamp correctly leave old rows unresolved. Failed probes are distinguishable from never-probed rows because the timestamp is populated even when identity is null.
- **Wave attach:** `probe_repo_identity` and timestamp capture occur at `waves.rs:469-482`, before `create_wave_with_spec_harness` opens the write transaction at `:585`; `AttachRepoIdentity` moves the paired result into the atomic folder/wave transaction correctly.
- **Frontend schemas:** `purpose: z.string().nullable().default(null)` mirrors `workflow_id`, accepts missing legacy payloads, and remains forward-compatible with server-minted string purposes.

## Review constraints

Per request, this was a static review only. I did not build or run tests and made no code changes outside this report.
