# Spec Card Reset / Refresh Findings

Historical: authored before PR7c deleted `spawn_push_appserver` and the legacy per-card daemon. The mechanisms described below were the model at the time; current behavior is in `docs/architecture/410-shared-codex-daemon.md`.

## Scope

- This document follows the locked brief semantics for spec-card Refresh and Reset. (`docs/spec-card-reset-refresh-brief.md:1`)
- The investigation was source-read only; this output is the only file created for the task. (`docs/spec-card-reset-refresh-brief.md:102`)
- Refresh means a client-only reconnect of the existing terminal bridge. (`docs/spec-card-reset-refresh-brief.md:34`)
- Refresh must not call a server reset or modify daemon/thread/push-cursor state. (`docs/spec-card-reset-refresh-brief.md:34`)
- Reset means a destructive server-side replacement of the current spec daemon and codex thread. (`docs/spec-card-reset-refresh-brief.md:38`)
- Reset must keep the wave, report card, spec card row, overlays, and wave observation history. (`docs/spec-card-reset-refresh-brief.md:47`)
- Reset must discard the existing codex thread transcript and any in-flight turn. (`docs/spec-card-reset-refresh-brief.md:49`)
- The PR should include Playwright/UI validation for non-trivial frontend behavior. (`docs/spec-card-reset-refresh-brief.md:78`)
- The PR should include Rust tests for backend lifecycle behavior. (`docs/spec-card-reset-refresh-brief.md:87`)

## Verdict

- Refresh is implementable without backend changes because `XtermView` already owns a dormant reconnect key. (`web/src/XtermView.tsx:227`)
- Refresh can reuse the existing `/api/terminals/:id` WebSocket attach path. (`web/src/XtermView.tsx:401`)
- Refresh does not need a new replay protocol because the server already returns render snapshots on attach. (`web/src/XtermView.tsx:527`)
- Reset is not currently implemented because the wave routes expose create, read, report, and cove-scoped listing only. (`crates/calm-server/src/routes/waves.rs:94`)
- Reset is also absent from the codex-card routes, which currently expose only plain user codex-card creation. (`crates/calm-server/src/routes/codex_cards.rs:53`)
- Reset should be added as a small backend route plus shared helper extraction from the current create-wave spec boot path. (`crates/calm-server/src/routes/waves.rs:814`)
- Reset must preserve the existing spec card row because cards carry stable `id`, `wave_id`, `kind`, payload, and `deletable` fields. (`crates/calm-server/src/model.rs:452`)
- Reset must be authorized by server-side card role, not by frontend inference, because the `Card` wire type does not expose role. (`crates/calm-server/src/model.rs:452`)
- The frontend can infer the visible spec card today from `kind=codex` and `deletable=false`, but the backend must still verify `role='spec'`. (`web/src/types.ts:186`)
- The implementation should not introduce a generic terminal respawn because spec reset needs codex-thread replacement and spec-push cleanup too. (`crates/calm-server/src/spec_appserver.rs:1`)

## Current Spec Card Creation

- Wave creation pre-mints both the spec card id and report card id before entering the transaction. (`crates/calm-server/src/routes/waves.rs:369`)
- The spec card creation path builds codex-specific environment for the spec daemon. (`crates/calm-server/src/routes/waves.rs:401`)
- The transaction writes the wave, the spec codex card, the report card, and the layout overlay together. (`crates/calm-server/src/routes/waves.rs:440`)
- The spec card is created through `card_with_codex_create_tx` with `CardRole::Spec`. (`crates/calm-server/src/routes/waves.rs:502`)
- The spec card is created as non-deletable. (`crates/calm-server/src/routes/waves.rs:506`)
- The report card is created in the same create-wave transaction. (`crates/calm-server/src/routes/waves.rs:532`)
- The transaction emits `WaveUpdated`, `CardAdded` for the spec card, `CardAdded` for the report card, and `OverlaySet`. (`crates/calm-server/src/routes/waves.rs:620`)
- The raw MCP token is appended after commit through environment variables and is not persisted raw. (`crates/calm-server/src/routes/waves.rs:636`)
- The push-only spec path boots an app-server, starts a codex thread, persists app-server fields, parks the handle, then spawns the terminal daemon in resume mode. (`crates/calm-server/src/routes/waves.rs:711`)
- Spec boot failure is currently non-fatal to wave creation. (`crates/calm-server/src/routes/waves.rs:719`)
- The current create-wave code logs spec boot errors but still returns the created wave payload. (`crates/calm-server/src/routes/waves.rs:734`)
- The `spawn_push_appserver` helper already performs the fresh-thread part reset needs. (`crates/calm-server/src/routes/waves.rs:814`)
- `spawn_push_appserver` persists `codex_thread_id`, `appserver_sock`, process-group stamps, and `push_watermark`. (`crates/calm-server/src/routes/waves.rs:907`)
- `spawn_push_appserver` emits a `CardUpdated` after writing app-server runtime payload fields. (`crates/calm-server/src/routes/waves.rs:1000`)
- `spawn_push_appserver` installs the durable watermark sink before parking the handle. (`crates/calm-server/src/routes/waves.rs:1033`)
- `spawn_push_appserver` installs the durable queue persist hooks before parking the handle. (`crates/calm-server/src/routes/waves.rs:1065`)
- The parked spec-push handle is keyed by wave id. (`crates/calm-server/src/routes/waves.rs:1086`)
- The terminal-facing daemon is spawned with `seed_and_spawn_spec_daemon` after app-server setup. (`crates/calm-server/src/routes/waves.rs:768`)

## Card And Terminal Model

- `card_with_codex_create_tx` atomically creates a codex card and a terminal row. (`crates/calm-server/src/db/sqlite.rs:1178`)
- `card_with_codex_create_tx` receives the card role and deletable flag as parameters. (`crates/calm-server/src/db/sqlite.rs:1208`)
- The card row stores `kind='codex'`, the supplied role, and the supplied deletable value. (`crates/calm-server/src/db/sqlite.rs:1242`)
- The terminal row is created with program `codex`. (`crates/calm-server/src/db/sqlite.rs:1257`)
- The card payload stores the generated `terminal_id`. (`crates/calm-server/src/db/sqlite.rs:1271`)
- The card payload may store `cwd`, `prompt`, and icon colors. (`crates/calm-server/src/db/sqlite.rs:1276`)
- The card payload does not expose `role`. (`crates/calm-server/src/db/sqlite.rs:1271`)
- `CardRole::Spec` exists as a persisted role variant. (`crates/calm-server/src/model.rs:18`)
- `CardRole` serializes to lowercase strings including `spec`. (`crates/calm-server/src/model.rs:42`)
- The `Card` wire model has no role field. (`crates/calm-server/src/model.rs:452`)
- The generated frontend `Card` type also has no role field. (`web/src/api/generated-events.ts:40`)
- The generated frontend `CardRole` union exists separately from `Card`. (`web/src/api/generated-events.ts:69`)
- The frontend codex-card data type has `terminalId` but no spec-role marker. (`web/src/types.ts:113`)
- `WaveCardSlot` marks kernel-owned cards through `deletable?: boolean`. (`web/src/types.ts:186`)
- The wave grid suppresses close controls when `deletable === false`. (`web/src/WaveGrid.tsx:248`)
- The wave router maps backend cards into wave card slots and carries `deletable`. (`web/src/app/router.tsx:336`)
- The card registry passes `onClose` into card components when a slot is closable. (`web/src/cards/registry.ts:117`)
- `CardHead` renders a close button only when `onClose` is supplied. (`web/src/cards/CardHead.tsx:63`)

## Frontend Codex Card

- The codex-card file documents that the backend spawns codex under a PTY and streams hooks over the card topic. (`web/src/cards/builtins/codex.tsx:1`)
- The codex-card file lazy-loads `XtermView`. (`web/src/cards/builtins/codex.tsx:40`)
- The codex payload schema accepts `terminal_id`, prompt, model, cwd, and icon colors. (`web/src/cards/builtins/codex.tsx:44`)
- The codex payload schema does not accept or expose `codex_thread_id`. (`web/src/cards/builtins/codex.tsx:44`)
- `CodexCardImpl` reads the card id, title, theme, FSM label, role, exit state, and terminal id. (`web/src/cards/builtins/codex.tsx:146`)
- `CodexCardImpl` subscribes to the card topic and listens for hook events and overlay status events. (`web/src/cards/builtins/codex.tsx:208`)
- `CodexCardImpl` renders the card title and status content through `CardHead`. (`web/src/cards/builtins/codex.tsx:253`)
- `CodexCardImpl` renders `XtermView` only when `terminalId` is present. (`web/src/cards/builtins/codex.tsx:292`)
- `CodexCardImpl` passes `terminalId`, theme, role callback, and exit callback into `XtermView`. (`web/src/cards/builtins/codex.tsx:296`)
- `CodexEntry.fromKernel` maps backend `kind='codex'` cards into frontend codex cards. (`web/src/cards/builtins/codex.tsx:356`)
- `CodexEntry.fromKernel` maps payload `terminal_id` to frontend `terminalId`. (`web/src/cards/builtins/codex.tsx:378`)
- `CardHead` already has a flexible `status` slot. (`web/src/cards/CardHead.tsx:29`)
- `CardHead` already has a `children` slot that can hold card-specific header controls. (`web/src/cards/CardHead.tsx:39`)
- The shared `IconButton` component already supports glyph, label, tone, and click handler. (`web/src/pages/_shared.tsx:11`)
- The shared `DeleteButton` already migrated destructive confirmation to `ConfirmDialog`. (`web/src/pages/_shared.tsx:56`)
- `DeleteButton` awaits asynchronous destructive work before closing pending state. (`web/src/pages/_shared.tsx:72`)

## Refresh Mechanics

- `XtermView` currently accepts only `terminalId`, `theme`, `onRoleChange`, and `onExitChange` props. (`web/src/XtermView.tsx:62`)
- `XtermView` describes the direct bridge to `/api/terminals/:id`. (`web/src/XtermView.tsx:139`)
- `XtermView` already has `reconnectKey` state. (`web/src/XtermView.tsx:227`)
- The current code voids `setReconnectKey`, so no caller can trigger reconnect today. (`web/src/XtermView.tsx:235`)
- The heavy terminal/WebSocket effect depends on `terminalId` and `reconnectKey`. (`web/src/XtermView.tsx:840`)
- A refresh implementation can expose an imperative `refresh()` that bumps `reconnectKey`. (`web/src/XtermView.tsx:227`)
- Reconnect teardown already disconnects the `ResizeObserver`. (`web/src/XtermView.tsx:781`)
- Reconnect teardown already disposes the xterm data subscription. (`web/src/XtermView.tsx:783`)
- Reconnect teardown already closes the WebSocket. (`web/src/XtermView.tsx:785`)
- Reconnect teardown already disposes the xterm instance. (`web/src/XtermView.tsx:789`)
- Reconnect teardown clears only its own test buffer-dump hook by terminal id. (`web/src/XtermView.tsx:793`)
- Reconnect teardown clears `termRef` only if it still owns the current terminal. (`web/src/XtermView.tsx:799`)
- Reconnect teardown clears `sendRef` only if it still owns the current sender. (`web/src/XtermView.tsx:804`)
- Reconnect teardown clears the parent role pill before the next attach. (`web/src/XtermView.tsx:811`)
- Reconnect teardown resets the live exit mirror. (`web/src/XtermView.tsx:816`)
- Theme updates are applied in place and do not rebuild the WebSocket. (`web/src/XtermView.tsx:826`)
- The WebSocket URL is built from the browser location and terminal id. (`web/src/XtermView.tsx:401`)
- On open, the client sends `ClientHello` with the same `terminal_id`. (`web/src/XtermView.tsx:467`)
- `ClientHello` requests no initial scrollback and no resume cursor. (`web/src/XtermView.tsx:471`)
- `ClientHello` sends an owner role hint. (`web/src/XtermView.tsx:482`)
- `ServerHello` restores role, status, and snapshot content on attach. (`web/src/XtermView.tsx:527`)
- `ServerHello` writes snapshot scrollback before snapshot data. (`web/src/XtermView.tsx:547`)
- `RenderPatch` appends VT data to the terminal. (`web/src/XtermView.tsx:552`)
- `RenderSnapshot` clears and rewrites the terminal from a snapshot. (`web/src/XtermView.tsx:561`)
- `TerminalExited` updates the visible exit state. (`web/src/XtermView.tsx:594`)
- Refresh therefore resets only the browser xterm/WS attachment and not the PTY child. (`web/src/XtermView.tsx:781`)

## Terminal Bridge

- The terminal WebSocket route is `/api/terminals/:id`. (`crates/calm-server/src/ws/terminal.rs:64`)
- The terminal WebSocket bridge is intentionally thin and delegates replay/cursors to the daemon attach layer. (`crates/calm-server/src/ws/terminal.rs:1`)
- The upgrade path resolves a live renderer before handling the socket. (`crates/calm-server/src/ws/terminal.rs:68`)
- The live-renderer lookup loads the terminal row by id. (`crates/calm-server/src/ws/terminal.rs:114`)
- If a renderer registry entry already exists, the bridge returns it as live. (`crates/calm-server/src/ws/terminal.rs:124`)
- If the terminal row has an exit code and no renderer entry, the bridge reports child-exited. (`crates/calm-server/src/ws/terminal.rs:130`)
- If no renderer entry exists, the bridge probes the supervisor before lazy reattach. (`crates/calm-server/src/ws/terminal.rs:141`)
- Lazy reattach is allowed only when the supervisor confirms the PTY is still live. (`crates/calm-server/src/ws/terminal.rs:172`)
- The bridge does not respawn a missing child for an exited terminal. (`crates/calm-server/src/ws/terminal.rs:124`)
- `handle_renderer` creates a per-client pump context with render plane, owner registry, supervisor channel, session id, and terminal id. (`crates/calm-server/src/ws/terminal.rs:192`)
- The bridge sanitizes client hello frames to the route terminal id. (`crates/calm-server/src/ws/terminal.rs:286`)
- The client pump requires the first client frame to be `ClientHello`. (`crates/calm-server/src/terminal_renderer/client_pump.rs:41`)
- The client pump rebuilds `ServerHello` from the render plane on attach. (`crates/calm-server/src/terminal_renderer/client_pump.rs:83`)
- The client pump sends `TerminalExited` after `ServerHello` when exit is already recorded. (`crates/calm-server/src/terminal_renderer/client_pump.rs:100`)
- The client pump sends `RenderSnapshot` if the broadcast receiver lags. (`crates/calm-server/src/terminal_renderer/client_pump.rs:142`)
- This makes Refresh safe even after short client-side packet loss. (`crates/calm-server/src/terminal_renderer/client_pump.rs:142`)

## Renderer Lifecycle

- `spawn_terminal_for` is the public route helper that starts a terminal renderer for a card terminal. (`crates/calm-server/src/routes/terminal.rs:54`)
- `spawn_terminal_for` delegates to `spawn_terminal_with_parts`. (`crates/calm-server/src/routes/terminal.rs:61`)
- `spawn_terminal_with_parts` uses the configured supervisor socket. (`crates/calm-server/src/routes/terminal.rs:80`)
- `spawn_terminal_with_parts` asks the renderer registry to ensure a renderer for the terminal id. (`crates/calm-server/src/routes/terminal.rs:100`)
- The renderer config stores terminal id, dimensions, shell command, cwd, and supervisor socket. (`crates/calm-server/src/terminal_renderer/mod.rs:51`)
- `TerminalRendererRegistry::ensure` returns an existing renderer when one is registered for the terminal id. (`crates/calm-server/src/terminal_renderer/mod.rs:144`)
- `TerminalRendererRegistry::ensure` inserts a new renderer only after `ensure_entry` succeeds. (`crates/calm-server/src/terminal_renderer/mod.rs:176`)
- `drop_entry` removes the renderer entry before terminating the process. (`crates/calm-server/src/terminal_renderer/mod.rs:250`)
- `drop_entry` sends `Term` and then `Kill` through the supervisor. (`crates/calm-server/src/terminal_renderer/mod.rs:259`)
- `ensure_entry` sends `EnsureProc` to the supervisor. (`crates/calm-server/src/terminal_renderer/mod.rs:270`)
- `ensure_entry` persists the spawned pid to the terminal row. (`crates/calm-server/src/terminal_renderer/mod.rs:290`)
- `ensure_entry` attaches the render plane to the supervisor PTY stream. (`crates/calm-server/src/terminal_renderer/mod.rs:322`)
- `reap_terminal_artifacts` exists as a shared cleanup helper. (`crates/calm-server/src/terminal_sweeper.rs:220`)
- `reap_terminal_artifacts` leaves row deletion to the caller. (`crates/calm-server/src/terminal_sweeper.rs:225`)
- If a renderer entry exists, `reap_terminal_artifacts` sends `Term` and calls `renderer.drop_entry`. (`crates/calm-server/src/terminal_sweeper.rs:241`)
- If no renderer entry exists, `reap_terminal_artifacts` falls back to pid-based cleanup. (`crates/calm-server/src/terminal_sweeper.rs:255`)
- Reset should reuse `reap_terminal_artifacts` rather than deleting the terminal row. (`crates/calm-server/src/terminal_sweeper.rs:220`)
- Reset should clear stale terminal exit metadata before or immediately after respawn because the UI mirrors terminal exit state. (`web/src/cards/builtins/codex.tsx:180`)

## Spec Push Runtime

- The spec-card runtime uses two processes: `codex app-server --listen` and terminal-facing `codex resume <thread_id> --remote`. (`crates/calm-server/src/spec_appserver.rs:1`)
- `SpecPushHandle` is parked in a registry keyed by wave id. (`crates/calm-server/src/spec_appserver.rs:25`)
- The app-server owns a process group. (`crates/calm-server/src/spec_appserver.rs:90`)
- Process-group identity is persisted because `kill(-pgid)` is load-bearing. (`crates/calm-server/src/spec_appserver.rs:90`)
- `SpecPushPhase::Resumed` exists to protect boot-resume catch-up from treating a resumed thread as idle. (`crates/calm-server/src/spec_appserver.rs:274`)
- `PushOutcome::Issued` advances the durable watermark. (`crates/calm-server/src/spec_appserver.rs:349`)
- `PushOutcome::Enqueued` must not advance the durable watermark. (`crates/calm-server/src/spec_appserver.rs:349`)
- `SpecPushHandle::drop` signals the process group with SIGTERM. (`crates/calm-server/src/spec_appserver.rs:1224`)
- `SpecPushHandle::drop` aborts the consumer and reconciler tasks. (`crates/calm-server/src/spec_appserver.rs:1239`)
- `read_process_start_time` reads the process start-time stamp. (`crates/calm-server/src/spec_appserver.rs:1248`)
- `read_boot_id` reads the boot-id stamp. (`crates/calm-server/src/spec_appserver.rs:1320`)
- `verify_owned_pid` checks pid, start time, and boot id. (`crates/calm-server/src/spec_appserver.rs:1348`)
- `signal_process_group` sends signals to the negative process group id. (`crates/calm-server/src/spec_appserver.rs:1401`)
- `SpecPushRegistry` is keyed by `WaveId`. (`crates/calm-server/src/spec_appserver.rs:1438`)
- Parking a handle can run install aspects before the handle becomes visible. (`crates/calm-server/src/spec_appserver.rs:1468`)
- Reset must remove the old registry handle so drop behavior cannot keep an old app-server alive. (`crates/calm-server/src/spec_appserver.rs:1224`)
- `reap_spec_push` removes the registry handle, signals the app-server process group, and cleans the socket directory. (`crates/calm-server/src/terminal_sweeper.rs:341`)
- `reap_spec_push` uses the same process-group kill helper as the rest of the app-server runtime. (`crates/calm-server/src/terminal_sweeper.rs:304`)
- Reset should call `reap_spec_push` before minting the fresh thread. (`crates/calm-server/src/terminal_sweeper.rs:341`)
- Reset should keep the process-group identity checks used by takeover when touching stale persisted pids. (`crates/calm-server/src/lib.rs:925`)

## Boot Takeover And Recovery

- Startup takeover scans spec cards with `codex_thread_id` and eligible wave lifecycle. (`crates/calm-server/src/db/sqlite.rs:1841`)
- The takeover scan projects the persisted thread id, app-server socket, process group, start time, boot id, and push watermark. (`crates/calm-server/src/db/sqlite.rs:1878`)
- Boot takeover excludes cards marked `appserver_needs_initial_prompt`. (`crates/calm-server/src/db/sqlite.rs:1897`)
- Boot takeover is invoked before the listener is bound. (`crates/calm-server/src/lib.rs:248`)
- The current takeover implementation always respawns app-server state for a persisted thread. (`crates/calm-server/src/spec_appserver.rs:1821`)
- The current takeover implementation removed the old live-app-server adoption path. (`crates/calm-server/src/spec_appserver.rs:1975`)
- The `main.rs` comment about reusing a live app-server is stale relative to the current implementation. (`crates/calm-server/src/main.rs:54`)
- Empty-goal bootstrap can replace `codex_thread_id` when the old thread had no rollout. (`crates/calm-server/src/lib.rs:451`)
- Empty-goal bootstrap persists a fresh `codex_thread_id` and marks `appserver_needs_initial_prompt`. (`crates/calm-server/src/db/sqlite.rs:2418`)
- Normal takeover post-respawn persistence updates only app-server fields and not the thread id or watermark. (`crates/calm-server/src/lib.rs:1358`)
- `spec_card_set_appserver_after_takeover` also avoids touching `codex_thread_id` and `push_watermark`. (`crates/calm-server/src/db/sqlite.rs:2459`)
- `spec_card_clear_push_state` removes thread id, app-server fields, watermark, and initial-prompt marker. (`crates/calm-server/src/db/sqlite.rs:2506`)
- Reset needs a new persistence helper because existing helpers are either takeover-only or clear-only. (`crates/calm-server/src/db/sqlite.rs:2459`)
- `register_and_catch_up` installs sinks, parks the handle, and catches up events since durable watermark. (`crates/calm-server/src/lib.rs:1387`)
- `register_and_catch_up` rehydrates persisted queue rows before replaying event-log catch-up. (`crates/calm-server/src/lib.rs:1482`)
- `register_and_catch_up` uses the per-wave push lock while seeding cursor, parking, and catch-up. (`crates/calm-server/src/lib.rs:1519`)
- Reset should reuse the same locking discipline to avoid racing live event push. (`crates/calm-server/src/lib.rs:1519`)

## Push Cursor And Queue

- The event cursor cache is keyed by spec card id. (`crates/calm-server/src/event_cursor.rs:1`)
- The cursor tracks the highest event id acted on for that spec card. (`crates/calm-server/src/event_cursor.rs:16`)
- The durable cursor survives restart and advances only after successful delivery. (`crates/calm-server/src/event_cursor.rs:22`)
- `spec_card_set_push_watermark` persists `payload.push_watermark` on the spec card. (`crates/calm-server/src/db/mod.rs:716`)
- `spec_card_set_push_watermark` is documented as card-id scoped. (`crates/calm-server/src/db/mod.rs:721`)
- The durable spec-push queue table is keyed by card id. (`crates/calm-server/migrations/0022_spec_push_queue.sql:59`)
- Queue rows are deleted automatically only when the card row is deleted. (`crates/calm-server/migrations/0022_spec_push_queue.sql:30`)
- Queue rows are out-of-sync-domain and do not have a public event variant. (`crates/calm-server/migrations/0022_spec_push_queue.sql:44`)
- The queue stores event envelope ids. (`crates/calm-server/migrations/0022_spec_push_queue.sql:37`)
- Enqueue inserts are exposed through `spec_card_enqueue_observation`. (`crates/calm-server/src/db/sqlite.rs:2541`)
- Queue reads are exposed through `spec_card_queued_observations`. (`crates/calm-server/src/db/sqlite.rs:2575`)
- Queue dequeue is exposed through `spec_card_dequeue_observations`. (`crates/calm-server/src/db/sqlite.rs:2603`)
- The dispatcher seeds the soft cursor from the persisted watermark. (`crates/calm-server/src/dispatcher.rs:166`)
- The dispatcher can reset the soft cursor to the durable watermark. (`crates/calm-server/src/dispatcher.rs:180`)
- Spec push is serialized with a per-wave lock. (`crates/calm-server/src/dispatcher.rs:205`)
- `push_to_spec_locked` resolves the spec card for the wave and dedups by cursor. (`crates/calm-server/src/dispatcher.rs:1050`)
- If no live spec handle exists, push does not bump the cursor. (`crates/calm-server/src/dispatcher.rs:1088`)
- On push error, durable watermark is not persisted. (`crates/calm-server/src/dispatcher.rs:1138`)
- When a turn is issued successfully, the dispatcher persists the new watermark. (`crates/calm-server/src/dispatcher.rs:1188`)
- When an observation is enqueued, the dispatcher bumps only the in-memory cursor. (`crates/calm-server/src/dispatcher.rs:1212`)
- Reset should keep the old durable watermark by default because the cursor is scoped to the spec card, not the codex thread. (`crates/calm-server/src/event_cursor.rs:1`)
- Reset should preserve queued observations by default because the queue is scoped to the spec card and survives card-row retention. (`crates/calm-server/migrations/0022_spec_push_queue.sql:59`)
- Preserved queued observations should replay into the new thread after reset only if they were not already watermarked. (`crates/calm-server/src/lib.rs:1482`)
- Advancing the reset watermark past queued rows would violate the persist-first queue contract. (`crates/calm-server/tests/inv_03_queue_persist_first.rs:1`)
- Replaying all historical event-log rows from zero would violate the current card-scoped cursor meaning. (`crates/calm-server/src/event_cursor.rs:16`)
- The least surprising reset default is therefore a clean codex transcript plus preserved card-scoped undelivered observations. (`crates/calm-server/migrations/0022_spec_push_queue.sql:21`)

## Reset State Matrix

| State | Current Owner | Reset Behavior | Anchor |
| --- | --- | --- | --- |
| Wave row | `waves` table via create-wave transaction | Preserve row | `crates/calm-server/src/routes/waves.rs:440` |
| Wave title | wave model and prompt source | Preserve title | `crates/calm-server/src/routes/waves.rs:502` |
| Wave lifecycle | wave row and takeover filter | Preserve lifecycle | `crates/calm-server/src/db/sqlite.rs:1841` |
| Spec card row | `cards` table with `role='spec'` | Preserve card id | `crates/calm-server/src/db/sqlite.rs:1242` |
| Spec card deletable flag | card row | Preserve non-deletable flag | `crates/calm-server/src/routes/waves.rs:506` |
| Spec terminal row | `terminals` row linked from card payload | Preserve row, respawn process | `crates/calm-server/src/db/sqlite.rs:1257` |
| Spec terminal id | card payload `terminal_id` | Prefer preserve id | `crates/calm-server/src/db/sqlite.rs:1271` |
| Old PTY child | renderer/supervisor | Kill before respawn | `crates/calm-server/src/terminal_sweeper.rs:241` |
| Old xterm browser view | frontend component state | Reconnect after reset | `web/src/XtermView.tsx:781` |
| Old app-server | `SpecPushRegistry` + process group | Kill via `reap_spec_push` | `crates/calm-server/src/terminal_sweeper.rs:341` |
| Old codex thread | card payload runtime field | Replace with fresh thread id | `crates/calm-server/src/routes/waves.rs:907` |
| Report card row | create-wave transaction | Preserve row | `crates/calm-server/src/routes/waves.rs:532` |
| Report edited state | report card payload/event model | Preserve card payload | `crates/calm-server/src/routes/waves.rs:532` |
| Layout overlay | overlay event | Preserve overlay | `crates/calm-server/src/routes/waves.rs:620` |
| Push watermark | spec card payload | Preserve durable value | `crates/calm-server/src/db/mod.rs:716` |
| Push queue rows | `spec_push_queue` by card id | Preserve rows | `crates/calm-server/migrations/0022_spec_push_queue.sql:59` |
| In-memory soft cursor | dispatcher cache | Reset to durable watermark | `crates/calm-server/src/dispatcher.rs:180` |
| In-flight turn | old app-server handle | Discard by killing process group | `crates/calm-server/src/spec_appserver.rs:1224` |
| Raw MCP token | post-commit environment only | Regenerate or use existing token policy | `crates/calm-server/src/routes/waves.rs:636` |

## UI Placement

- The wave page already has top-level wave actions near lifecycle and delete controls. (`web/src/pages/Wave.tsx:260`)
- The requested affordance is card-local because Refresh and Reset affect one spec card terminal. (`web/src/cards/builtins/codex.tsx:253`)
- `CardHead` exposes the right header region through `children` and `status`. (`web/src/cards/CardHead.tsx:39`)
- The spec card should place Refresh and Reset near the status controls, not in the global wave header. (`web/src/cards/builtins/codex.tsx:253`)
- Refresh should use a neutral icon button because it is non-destructive. (`web/src/pages/_shared.tsx:11`)
- Reset should use a danger-toned icon button because it kills processes and discards a thread. (`web/src/pages/_shared.tsx:11`)
- The frontend should render those buttons only for non-deletable codex cards or after adding an explicit spec flag. (`web/src/WaveGrid.tsx:248`)
- Adding explicit role to the card wire model would be cleaner but touches broader API surface. (`crates/calm-server/src/model.rs:452`)
- A PR-sized first pass can infer spec in the adapter from `kind='codex'` plus `deletable=false`. (`web/src/cards/builtins/codex.tsx:356`)
- The backend route must still reject non-spec card ids. (`crates/calm-server/src/model.rs:18`)

## Confirmation Dialog

- `ConfirmDialog` is cancel-safe by default. (`web/src/ui/ConfirmDialog/ConfirmDialog.tsx:1`)
- `ConfirmDialog` routes outside click and Escape to cancel. (`web/src/ui/ConfirmDialog/ConfirmDialog.tsx:1`)
- `ConfirmDialog` starts focus on the cancel button. (`web/src/ui/ConfirmDialog/ConfirmDialog.tsx:76`)
- Destructive confirm buttons receive the warning class. (`web/src/ui/ConfirmDialog/ConfirmDialog.tsx:118`)
- The underlying dialog contract includes focus trap, initial focus, restore focus, and inert background. (`web/src/ui/Dialog/Dialog.tsx:15`)
- Reset should use `ConfirmDialog` rather than adding a custom modal. (`web/src/ui/ConfirmDialog/ConfirmDialog.tsx:46`)
- The reset dialog copy must state that the current codex thread transcript and any running turn will be discarded. (`docs/spec-card-reset-refresh-brief.md:49`)
- The reset dialog copy must state that wave/report/spec-card state will remain. (`docs/spec-card-reset-refresh-brief.md:47`)
- The confirm action should remain pending while the backend reset route runs. (`web/src/pages/_shared.tsx:72`)

## Backend Route Shape

- A card-id route is natural because the frontend card already has the spec card id. (`web/src/cards/builtins/codex.tsx:146`)
- A route like `POST /api/cards/{card_id}/spec/reset` keeps reset scoped to the card being displayed. (`crates/calm-server/src/routes/cards.rs:445`)
- The route must first load the card and verify it is role `Spec`. (`crates/calm-server/src/model.rs:18`)
- Existing direct card delete rejects non-deletable cards, showing server-side card ownership checks already exist. (`crates/calm-server/src/routes/cards.rs:445`)
- Reset should not reuse card delete because delete cascades card/terminal state. (`crates/calm-server/src/routes/cards.rs:459`)
- Reset should use the wave id from the spec card row for registry and dispatcher locking. (`crates/calm-server/src/model.rs:452`)
- Reset should acquire the per-wave push lock before tearing down and replacing the app-server. (`crates/calm-server/src/dispatcher.rs:205`)
- Reset should call `reap_spec_push` for the old app-server handle. (`crates/calm-server/src/terminal_sweeper.rs:341`)
- Reset should call `reap_terminal_artifacts` for the existing terminal id. (`crates/calm-server/src/terminal_sweeper.rs:220`)
- Reset should spawn a fresh app-server thread through extracted `spawn_push_appserver` logic. (`crates/calm-server/src/routes/waves.rs:814`)
- Reset should respawn the terminal daemon with `codex resume <new_thread_id> --remote`. (`crates/calm-server/src/spec_card.rs:652`)
- Reset should return the updated card or a compact reset result including `card_id` and `terminal_id`. (`crates/calm-server/src/model.rs:452`)
- Reset should emit a `CardUpdated` if it mutates card payload runtime fields. (`crates/calm-server/src/routes/waves.rs:1000`)
- Reset should not emit `CardAdded` because the card row survives. (`crates/calm-server/src/routes/waves.rs:620`)
- Reset should not emit `WaveUpdated` unless wave fields change. (`crates/calm-server/src/routes/waves.rs:620`)
- Reset should not touch the report card row. (`crates/calm-server/src/routes/waves.rs:532`)
- Reset should not touch overlays. (`crates/calm-server/src/routes/waves.rs:620`)

## Backend Helper Needs

- The current fresh app-server boot helper is private to the wave routes module. (`crates/calm-server/src/routes/waves.rs:814`)
- Reset needs that helper or its core logic moved to a spec service module. (`crates/calm-server/src/routes/waves.rs:814`)
- The extracted helper should preserve initial prompt handling used by create-wave. (`crates/calm-server/src/routes/waves.rs:892`)
- The extracted helper should preserve WatermarkSink installation. (`crates/calm-server/src/routes/waves.rs:1033`)
- The extracted helper should preserve QueuePersist installation. (`crates/calm-server/src/routes/waves.rs:1065`)
- The extracted helper should preserve registry parking aspects. (`crates/calm-server/src/routes/waves.rs:1086`)
- The extracted helper should preserve per-card app-server socket path derivation. (`crates/calm-server/src/state.rs:497`)
- The extracted helper should preserve per-card CODEX_HOME seeding. (`crates/calm-server/src/routes/waves.rs:849`)
- Reset needs a repo helper to replace `codex_thread_id` and app-server fields atomically for an existing spec card. (`crates/calm-server/src/db/sqlite.rs:2418`)
- Reset should not use `spec_card_set_empty_goal_bootstrap_state` because reset is not necessarily an empty-goal recovery. (`crates/calm-server/src/db/sqlite.rs:2418`)
- Reset should not use only `spec_card_set_appserver_after_takeover` because it does not write a new thread id. (`crates/calm-server/src/db/sqlite.rs:2459`)
- Reset should not use only `spec_card_clear_push_state` because that removes the durable watermark. (`crates/calm-server/src/db/sqlite.rs:2506`)
- Reset needs a terminal helper to clear stale terminal exit metadata if the existing terminal row is reused. (`crates/calm-server/src/ws/terminal.rs:130`)

## Failure Matrix

| Failure | Desired Behavior | Anchor |
| --- | --- | --- |
| Refresh WebSocket close races with new mount | Strict-mode guards keep new refs intact | `web/src/XtermView.tsx:799` |
| Refresh occurs after terminal has exited | Bridge reports child-exited if no renderer exists | `crates/calm-server/src/ws/terminal.rs:130` |
| Refresh loses broadcast frames | Client pump sends snapshot on lag | `crates/calm-server/src/terminal_renderer/client_pump.rs:142` |
| Reset target card is plain codex | Backend rejects because role is not Spec | `crates/calm-server/src/model.rs:18` |
| Reset target card is report card | Backend rejects because kind/role mismatch | `crates/calm-server/src/model.rs:18` |
| Reset while turn is running | Old process group is killed and old turn dies | `crates/calm-server/src/spec_appserver.rs:1224` |
| Reset while dispatcher pushes observation | Per-wave lock serializes reset with push | `crates/calm-server/src/dispatcher.rs:205` |
| Reset after old app-server already died | `reap_spec_push` removal/cleanup is already best-effort | `crates/calm-server/src/terminal_sweeper.rs:341` |
| Reset after old terminal already exited | `reap_terminal_artifacts` has pid fallback and row-preserving behavior | `crates/calm-server/src/terminal_sweeper.rs:255` |
| Fresh app-server spawn fails | Route should return failure after old runtime is gone | `crates/calm-server/src/routes/waves.rs:742` |
| Fresh terminal daemon spawn fails | Route should surface failure and leave app-server cleanup path explicit | `crates/calm-server/src/spec_card.rs:793` |
| Fresh thread start succeeds but persist fails | Helper should rollback child process group | `crates/calm-server/src/spec_appserver.rs:2000` |
| Fresh thread has no rollout | Existing no-rollout handling clears push state during takeover | `crates/calm-server/src/lib.rs:1202` |
| Socket path is stale | Existing recovery removes socket path before respawn | `crates/calm-server/src/spec_appserver.rs:1876` |
| PID reused by unrelated process | Ownership check uses start time and boot id | `crates/calm-server/src/spec_appserver.rs:1348` |
| Queued observations exist before reset | Keep rows and replay only unwatermarked observations | `crates/calm-server/migrations/0022_spec_push_queue.sql:59` |
| Reset button double-click | Frontend pending state should disable repeats | `web/src/pages/_shared.tsx:72` |
| User cancels reset dialog | ConfirmDialog calls cancel on dismiss | `web/src/ui/ConfirmDialog/ConfirmDialog.tsx:1` |

## Tests To Add

- Add a frontend test that `XtermView.refresh()` closes the current WebSocket and opens a new one to the same terminal endpoint. (`web/src/XtermView.tsx:401`)
- The refresh test should assert no HTTP reset endpoint is called. (`docs/spec-card-reset-refresh-brief.md:34`)
- Add a frontend card test that the Refresh button is visible only for the inferred spec codex card. (`web/src/WaveGrid.tsx:248`)
- Add a frontend card test that the Reset button opens `ConfirmDialog`. (`web/src/ui/ConfirmDialog/ConfirmDialog.tsx:46`)
- Add a frontend card test that canceling reset does not call the reset endpoint. (`web/src/ui/ConfirmDialog/ConfirmDialog.tsx:1`)
- Add a frontend card test that confirming reset awaits the reset endpoint and then refreshes the terminal view. (`web/src/pages/_shared.tsx:72`)
- Add a Rust route test that a plain codex card cannot be reset. (`crates/calm-server/src/routes/codex_cards.rs:264`)
- Add a Rust route test that reset preserves the card id and terminal id. (`crates/calm-server/src/db/sqlite.rs:1271`)
- Add a Rust route test that reset writes a new `codex_thread_id`. (`crates/calm-server/src/routes/waves.rs:907`)
- Add a Rust route test that reset preserves `push_watermark`. (`crates/calm-server/src/db/mod.rs:716`)
- Add a Rust route test that reset preserves `spec_push_queue` rows. (`crates/calm-server/migrations/0022_spec_push_queue.sql:59`)
- Add a Rust route test that reset calls the process-group reaper for the app-server path. (`crates/calm-server/src/terminal_sweeper.rs:341`)
- Add a Rust route test that reset calls terminal artifact cleanup without deleting the terminal row. (`crates/calm-server/src/terminal_sweeper.rs:220`)
- Add a startup/regression test that reset-installed handles still have WatermarkSink installed. (`crates/calm-server/tests/inv_06_startup_symmetry.rs:196`)
- Add an integration test mirroring INV-4 semantics for reset during a running turn. (`crates/calm-server/tests/inv_04_turn_phase_mutex.rs:1`)
- Socket/process-heavy reset tests may need the same external environment constraints as existing codex-e2e coverage. (`crates/calm-server/tests/inv_03_queue_persist_first.rs:37`)

## Existing Invariant Coverage

- INV-1 pins that abandoned boot takeover emits a persisted `SpecPushAbandoned` event. (`crates/calm-server/tests/inv_01_watermark_monotonic.rs:1`)
- INV-1 matters to reset because clearing or replacing push state must remain observable when delivery is abandoned. (`crates/calm-server/tests/inv_01_watermark_monotonic.rs:20`)
- INV-2 pins process-group teardown instead of pid-only teardown. (`crates/calm-server/tests/inv_02_killpg.rs:1`)
- INV-2 matters to reset because reset kills a live app-server process tree. (`crates/calm-server/tests/inv_02_killpg.rs:4`)
- INV-3 pins persist-first queue behavior. (`crates/calm-server/tests/inv_03_queue_persist_first.rs:1`)
- INV-3 matters to reset because queued observations are durable card-scoped state. (`crates/calm-server/tests/inv_03_queue_persist_first.rs:10`)
- INV-4 pins that resumed boot takeover enqueues first catch-up push instead of starting a concurrent turn. (`crates/calm-server/tests/inv_04_turn_phase_mutex.rs:1`)
- INV-4 matters to reset because reset must serialize turn replacement with event push. (`crates/calm-server/tests/inv_04_turn_phase_mutex.rs:17`)
- INV-5 pins pid ownership through pid, start time, and boot id. (`crates/calm-server/tests/inv_05_pid_ownership_strong.rs:1`)
- INV-5 matters to reset because reset should not kill a reused pid owned by another process. (`crates/calm-server/tests/inv_05_pid_ownership_strong.rs:25`)
- INV-6 pins startup symmetry and WatermarkSink installation for parked handles. (`crates/calm-server/tests/inv_06_startup_symmetry.rs:1`)
- INV-6 matters to reset because reset creates another parked handle path. (`crates/calm-server/tests/inv_06_startup_symmetry.rs:39`)

## Implementation Plan

1. Add an imperative refresh handle to `XtermView` by wrapping the component with `forwardRef` and `useImperativeHandle`. (`web/src/XtermView.tsx:155`)
2. Implement `refresh()` as a `setReconnectKey((n) => n + 1)` bump and remove the current `void setReconnectKey` placeholder. (`web/src/XtermView.tsx:235`)
3. Keep the effect dependency on `terminalId` and `reconnectKey` unchanged so refresh reuses existing teardown and attach behavior. (`web/src/XtermView.tsx:840`)
4. In `CodexCardImpl`, hold an `XtermView` ref and wire a neutral Refresh icon button to `ref.current?.refresh()`. (`web/src/cards/builtins/codex.tsx:296`)
5. Infer spec-card UI eligibility from codex card plus non-deletable slot data, or add an explicit adapter field if that is smaller in the local type flow. (`web/src/WaveGrid.tsx:248`)
6. Add a danger Reset icon button in the codex card header near the existing status region. (`web/src/cards/builtins/codex.tsx:253`)
7. Use `ConfirmDialog` for Reset confirmation with cancel default focus and destructive confirm styling. (`web/src/ui/ConfirmDialog/ConfirmDialog.tsx:76`)
8. Add a tiny frontend API helper for `POST /api/cards/{card_id}/spec/reset`. (`web/src/api/schemas.ts:119`)
9. After a successful reset response, call the same `XtermView.refresh()` handle so the browser attaches to the respawned PTY. (`web/src/XtermView.tsx:227`)
10. Add a backend route under cards or a new spec-card routes module for `POST /api/cards/{card_id}/spec/reset`. (`crates/calm-server/src/routes/cards.rs:445`)
11. Add a repo helper that loads card id, wave id, terminal id, role, payload, and deletable state for reset authorization. (`crates/calm-server/src/model.rs:452`)
12. Reject reset unless the card role is `CardRole::Spec` and the card kind is codex. (`crates/calm-server/src/model.rs:18`)
13. Acquire the per-wave push lock for the reset critical section. (`crates/calm-server/src/dispatcher.rs:205`)
14. Reap the existing spec-push app-server through `reap_spec_push`. (`crates/calm-server/src/terminal_sweeper.rs:341`)
15. Reap the existing terminal artifacts through `reap_terminal_artifacts` while preserving the terminal row. (`crates/calm-server/src/terminal_sweeper.rs:220`)
16. Extract the fresh-thread app-server boot logic from `spawn_push_appserver` into a shared spec runtime helper. (`crates/calm-server/src/routes/waves.rs:814`)
17. Preserve CODEX_HOME seeding, socket path derivation, thread start, lifecycle wait, sinks, and registry parking in the extracted helper. (`crates/calm-server/src/routes/waves.rs:849`)
18. Add a repo helper that replaces `codex_thread_id`, app-server sock, pgid, start time, and boot id without changing card id, terminal id, watermark, queue, or report state. (`crates/calm-server/src/db/sqlite.rs:2459`)
19. Clear stale terminal exit metadata when reusing the existing terminal row. (`crates/calm-server/src/ws/terminal.rs:130`)
20. Respawn the terminal daemon with `codex resume <new_thread_id> --remote unix://<sock>`. (`crates/calm-server/src/spec_card.rs:652`)
21. Emit `CardUpdated` if the reset path mutates spec-card runtime payload fields. (`crates/calm-server/src/routes/waves.rs:1000`)
22. Preserve durable `push_watermark` and `spec_push_queue` rows so only undelivered observations replay into the new thread. (`crates/calm-server/migrations/0022_spec_push_queue.sql:59`)
23. Reset the in-memory push cursor to the durable watermark after installing the fresh handle. (`crates/calm-server/src/dispatcher.rs:180`)
24. Add frontend refresh and reset interaction tests around WebSocket reconnect, confirm cancel, confirm submit, pending state, and endpoint failure. (`web/src/ui/ConfirmDialog/ConfirmDialog.tsx:46`)
25. Add backend route tests for authorization rejection, id preservation, fresh thread id, app-server cleanup, terminal cleanup, watermark preservation, and queue preservation. (`crates/calm-server/tests/inv_06_startup_symmetry.rs:92`)
26. Add a regression assertion that reset-created handles install the same queue persist hooks as create-wave and takeover handles. (`crates/calm-server/src/routes/waves.rs:1065`)
27. Add a regression assertion that reset-created handles install the same watermark sink as create-wave and takeover handles. (`crates/calm-server/src/routes/waves.rs:1033`)
28. Add a failure-path test where fresh app-server boot fails and the route returns an error without deleting wave or card rows. (`crates/calm-server/src/routes/waves.rs:742`)
29. Add a failure-path test where terminal daemon respawn fails and the response makes the degraded state explicit. (`crates/calm-server/src/spec_card.rs:793`)
30. Add a small manual QA checklist covering Refresh on running, exited, and reset-respawned terminals. (`crates/calm-server/src/ws/terminal.rs:124`)
31. Run targeted frontend tests, targeted Rust reset tests, existing INV-1 through INV-6 tests, and a manual process/sandbox-external reset smoke test before merging. (`crates/calm-server/tests/inv_01_watermark_monotonic.rs:1`)
