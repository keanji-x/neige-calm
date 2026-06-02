# Shared codex daemon architecture (issue #410)

## What changed

Pre-PR1, each codex-backed card spawned its own `codex app-server` child
process inside a per-card `CODEX_HOME` directory. With dozens of cards, the
kernel ran dozens of daemon processes and duplicated session-token, config, and
cache state across per-card homes. Typical disk use was 1-2 GB per card times N
cards, which made 20-50 GB calm workspaces easy to produce.

Post-PR8, one `codex app-server` runs alongside the kernel as
`SharedCodexAppServer` ([source](../../crates/calm-server/src/shared_codex_appserver.rs)).
Codex-backed cards open a thread on this shared daemon and run their TUI as
`codex resume <thread_id> --remote unix://<sock>`. The daemon's `CODEX_HOME`
lives at `<data_dir>/codex-home/` and card-to-thread ownership is persisted in
the `card_codex_threads` SQLite table
([migration](../../crates/calm-server/migrations/0025_card_codex_threads.sql)).

## Flags

Five flags control the rollout. After PR8 all are default-on in
`Config` ([source](../../crates/calm-server/src/config.rs)) and
`AppState::from_parts` fixture defaults
([source](../../crates/calm-server/src/state.rs)). Legacy paths remain behind
explicit `false` overrides for emergency rollback.

| Flag | Default | Off behavior |
|---|---|---|
| `shared_codex_appserver_enabled` | true (PR4) | Do not spawn the shared daemon; cards fall back to legacy spawn paths |
| `shared_codex_prompt_cards_enabled` | **true** (PR8) | Prompt user card create spawns standalone `codex <prompt>` PTY per legacy |
| `shared_codex_empty_cards_enabled` | **true** (PR8) | Empty user card create spawns standalone `codex` PTY per legacy |
| `shared_codex_spec_cards_enabled` | **true** (PR8) | Spec card create-wave spawns per-wave `codex app-server` per legacy |
| `shared_codex_worker_cards_enabled` | **true** (PR8) | Dispatcher worker spawns standalone `codex <prompt>` PTY per legacy |

When any card-kind flag is disabled at boot, `main.rs` emits a warning under
`shared_codex_daemon::flag` so production rollbacks are visible
([source](../../crates/calm-server/src/main.rs)).

## Data flow per card type

### Prompt user card (PR5)

1. `POST /api/codex-cards` receives a non-empty prompt.
2. The route calls `shared_codex.thread_start_for_card(card_id, Plain, ...)`,
   which starts a daemon thread, upserts `card_codex_threads`, and caches the
   thread-to-card mapping
   ([source](../../crates/calm-server/src/shared_codex_appserver.rs)).
3. The route calls `shared_codex.turn_start(thread_id, prompt)` to deliver turn
   1 ([source](../../crates/calm-server/src/routes/codex_cards.rs)).
4. The card payload is stamped with `codex_source: "shared"` and
   `codex_thread_id`.
5. The PTY spawn command is `codex resume <thread_id> --remote unix://<sock>`.

### Empty user card (PR6)

1. `POST /api/codex-cards` receives an empty prompt.
2. The route registers the card in `pending_codex_threads` with role `Plain`
   before spawning the PTY ([route](../../crates/calm-server/src/routes/codex_cards.rs),
   [registry](../../crates/calm-server/src/pending_codex_threads.rs)).
3. The PTY spawn command is `codex --remote unix://<sock>`, so the TUI
   fresh-starts the thread.
4. The daemon emits `thread/started`; the pending registry binds the event to
   the FIFO front entry.
5. The card payload is backfilled with `codex_thread_id`.

### Spec card / wave create (PR7b)

Non-empty title spec cards use the prompt-card pattern with `CardRole::Spec`
and rendered spec developer instructions. Empty title spec cards use the
empty-card pattern with `CardRole::Spec`; the developer instructions travel on
the TUI argv as `codex -c developer_instructions=... --remote ...`
([source](../../crates/calm-server/src/routes/waves.rs)).

### Worker card (PR7b-worker)

The dispatcher mints the worker card and then calls
`thread_start_for_card(Worker, ...)` to create the mapping. It starts the worker
turn with the worker prompt and spawns `codex resume <thread_id> --remote
unix://<sock>`. Worker cards are hands-free and do not interact with the
spec-push registry. Hook callbacks such as `task.completed` and `task.failed`
resolve `card_id` through the bridge identity
([source](../../crates/calm-server/src/dispatcher.rs)).

### Reset (PR7b-reset)

Shared spec-card reset mints a new thread and upserts `card_codex_threads`,
replacing the old mapping for that card. The old turn is interrupted with
`turn/interrupt`; the old thread remains loaded in the daemon because codex
0.135 has no close RPC
([source](../../crates/calm-server/src/routes/cards.rs)).

## `card_codex_threads` table (PR2 + PR2b)

Migration 0025 created `card_codex_threads`, keyed it by `card_id` with a unique
constraint, and backfilled existing spec-card payload thread IDs
([migration](../../crates/calm-server/migrations/0025_card_codex_threads.sql)).
The table stores `(thread_id, role, wave_id)` and is the authoritative boot
takeover read path for shared daemon threads
([takeover](../../crates/calm-server/src/lib.rs)). UPSERT semantics in
`card_codex_thread_upsert_tx` naturally support reset replacing one thread with
another ([source](../../crates/calm-server/src/db/sqlite.rs)).

## Followup gates closed (post-PR7b-reset)

| Gate | PR closing | Mechanism |
|---|---|---|
| #1 Runtime proxy hot-reload | PR-gates-easy | `mark_needs_respawn` records settings drift; next shared thread start reaps and respawns the daemon ([source](../../crates/calm-server/src/shared_codex_appserver.rs)) |
| #2 TTL-expire payload clear | PR-gates-easy | TTL expiry calls `drop_stale_entry`, matching dead-terminal expiry ([source](../../crates/calm-server/src/pending_codex_threads.rs)) |
| #3 Front-dead-then-continue race | PR-gate-front-dead | Stale FIFO front entries are dropped instead of reusing the same thread ID ([source](../../crates/calm-server/src/pending_codex_threads.rs)) |
| #4 Persist-failure rollback for empty pending | PR-gates-easy | Empty shared paths persist card state before `pending.register()` ([user cards](../../crates/calm-server/src/routes/codex_cards.rs), [spec cards](../../crates/calm-server/src/routes/waves.rs)) |
| #5 / #6 / #5b Orphan in-flight turn cleanup | PR-gates-orphan-thread | `turn/interrupt` is sent on card delete, spawn failure, and reset cleanup ([client](../../crates/calm-server/src/codex_appserver.rs), [reset](../../crates/calm-server/src/routes/cards.rs)) |

## Known limitations

- **codex 0.135 lacks `thread/close`**: interrupted threads remain loaded in
  shared daemon memory. PR7c will add a periodic daemon-respawn pulse or admin
  GC for accumulating cruft. Watch for codex 0.136 release.
- **Soft-deterministic FIFO attribution**: empty cards rely on PTY spawn order
  matching `thread/started` arrival order. Cross-attribution is prevented by
  gate #3 at the cost of occasional missed binds, recoverable via TTL and user
  retry.
- **Operator telemetry pending**: PR-gates-easy added structured tracing under
  `shared_codex_daemon::*` targets. Staging validation is recommended before
  this ships to production.

## Resume points

If issues are observed in production after the flag flip:

1. Set `CALM_SHARED_CODEX_<KIND>_CARDS_ENABLED=false` at the service level. The
   daemon stops minting new shared threads for that kind; existing shared cards
   continue to function.
2. PR8 and PR7c can be reverted individually. PR7c, which deletes legacy paths,
   carries higher rollback cost.
