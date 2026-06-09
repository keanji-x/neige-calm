# Fix-loop brief — PR #587 review findings

Address 4 findings from PR #587 review. Stay in worktree `581-pr1pr2`, branch `feat/581-pr1pr2-rename-card-runtime`. Do NOT touch unrelated code.

## F5 (priority) — `CardRuntimeView` must use typed enums, not strings

Currently `crates/calm-server/src/model.rs:455-475` declares `CardRuntimeView` with `kind: String`, `status: String`, `provider: Option<String>`. The PR also added helpers `runtime_kind_wire` / `agent_provider_wire` / `run_status_wire` in `crates/calm-server/src/runtime_lookup.rs:203-229` to convert enums to strings. These duplicate the `serde(rename)` already on the enums — two sources of truth.

Change:
- In `crates/calm-server/src/model.rs:455-475` change types to:
  - `pub kind: RuntimeKind` (import from `crate::runtime_repo`)
  - `pub status: RunStatus`
  - `pub provider: Option<AgentProvider>` (keep `#[serde(default, skip_serializing_if = "Option::is_none")]` + `#[ts(optional)]`)
- The other Optional<String> fields (`terminal_id`, `thread_id`, `session_id`, `source`, `thread_status`) stay String. `source` is a magic constant ("shared"); `thread_status` is a three-value string ("pending_thread_start", "failed_to_spawn", "started"). Leave them String for this PR; PR3 can tighten.
- In `crates/calm-server/src/runtime_lookup.rs`:
  - Delete the helpers `runtime_kind_wire`, `agent_provider_wire`, `run_status_wire` (around `:203-229`).
  - Update the construction of `CardRuntimeView` (around `:184-201`) to assign `kind: runtime.kind`, `status: runtime.status`, `provider: runtime.agent_provider` (the latter via `runtime.agent_provider.clone()` since AgentProvider is likely `Clone` — confirm by reading `runtime_repo.rs:1-50`).
- Regenerate via `npm run gen:api` from `web/`. Verify the generated TS surface for `CardRuntimeView` now uses `RuntimeKind` / `RunStatus` / `AgentProvider` types instead of bare `string`.
- Update tests in `crates/calm-server/tests/runtime_repo.rs:745-1000` to compare against typed enums:
  - For `RuntimeKind`: assertion like `assert_eq!(runtime.kind, RuntimeKind::CodexCard)` (use whatever the actual variant name is — look it up).
  - For `RunStatus`: similar (e.g. `RunStatus::Running`).
  - For `provider`: `Some(AgentProvider::Codex)` etc.
- If any test currently does `runtime.kind == "codex"` (string compare) update to enum compare.
- Update web zod schema in `web/src/api/schemas.ts` if `CardRuntimeView` is defined there with `z.string()` — make those fields `z.enum([...])` matching what generated.ts says, OR import from generated.ts. Match the existing pattern in that file.

## F4 — replay test asymmetry

Add legacy-kind coverage to `crates/calm-server/src/event.rs` around the `new_variants_round_trip_via_from_kind_and_payload` test (search for that fn name). In the same test loop, also exercise the legacy strings `codex.job_requested` / `terminal.job_requested` via `Event::from_kind_and_payload`, asserting:
- The deserialized event matches the new variant (`CodexWorkerRequested` / `TerminalWorkerRequested`).
- `evt.kind_tag()` returns the NEW string (`codex.worker_requested` / `terminal.worker_requested`).

If `from_kind_and_payload` builds an envelope internally and serde alias makes this work, the test should pass with a one-loop addition. If it doesn't compile or fails, that's evidence the alias path doesn't actually cover replay — surface it.

## F3 — terminology glossary

`docs/architecture/terminology-glossary.md:74` (or near that line — grep for `job_requested` in that file) still references old kind names. Update to `*.worker_requested`.

## F6 — doc-comment `CardRuntimeView` semantics

Add a Rust doc-comment on `CardRuntimeView` (in `crates/calm-server/src/model.rs:455` area) explaining:
- It is a LIVE projection from the `runtimes` table at fetch/serialize time.
- It is NOT part of the idempotency contract — across retries the runtime row may have advanced, so `Card.runtime` may differ between first POST and retry POST returning the same operation result.
- Future cleanup (#581 item 4) will remove the legacy payload-key projection; this typed view is the forward-compatible reader path.

## Validate
- `PATH=/home/kenji/.cargo/bin:$PATH cargo fmt --all --check`
- `PATH=/home/kenji/.cargo/bin:$PATH cargo clippy --workspace --all-targets -- -D warnings`
- `PATH=/home/kenji/.cargo/bin:$PATH cargo test -p calm-server --test runtime_repo`
- `PATH=/home/kenji/.cargo/bin:$PATH cargo test -p calm-server --lib event::tests::new_variants_round_trip_via_from_kind_and_payload`
- `PATH=/home/kenji/.cargo/bin:$PATH cargo test --workspace --no-fail-fast` (full)
- From `web/`: `PATH=/home/kenji/.cargo/bin:/home/kenji/.nvm/versions/node/v22.22.2/bin:$PATH npm run gen:api && npm test -- --run && npm run typecheck && npm run build`

Report the diff stat + which tests changed. Do NOT commit; I'll commit.
