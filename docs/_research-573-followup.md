# #573 follow-up research

## Issue 1 - Edit button visibility

- The pencil is rendered by `WaveReportCard`: `canEdit` is only `waveId !== null`, and the `canEdit && !editing` branch adds the edit button in the card header. See `web/src/cards/builtins/wave-report.tsx:486` and `web/src/cards/builtins/wave-report.tsx:505`.
- In non-editing mode, a real wave renders `WaveReportSidebar` and passes `ReadOnlyView` only as its fallback. See `web/src/cards/builtins/wave-report.tsx:543`.
- `WaveReportSidebarState` owns `selectedPath` internally. It starts as `null`, file click/Enter sets it, and the value is passed to `WaveFileViewer`. See `web/src/cards/builtins/wave-report-sidebar.tsx:24`, `web/src/cards/builtins/wave-report-sidebar.tsx:100`, `web/src/cards/builtins/wave-report-sidebar.tsx:270`, and `web/src/cards/builtins/wave-report-sidebar.tsx:131`.
- When `selectedPath` is null, `WaveFileViewer` shows the fallback report view. When a file is selected, it fetches that file and renders read-only Markdown or a `CodePane`. See `web/src/cards/builtins/wave-report-sidebar.tsx:322`, `web/src/cards/builtins/wave-report-sidebar.tsx:324`, `web/src/cards/builtins/wave-report-sidebar.tsx:356`, and `web/src/cards/builtins/wave-report-sidebar.tsx:366`.
- Result: after phase 2, selecting `.payload.json`, `conversation.md`, `events.json`, `wave.json`, etc. replaces the right pane with a read-only projection, but the header pencil still opens the report editor. That affordance only makes sense for the default report view (`selectedPath === null`) and probably explicit `report.md`.

Fix shapes:

1. Recommended: lift/control `selectedPath` in `WaveReportCard`, pass `selectedPath` plus `onSelectedPathChange` into `WaveReportSidebar`, and gate the pencil with `waveId !== null && (selectedPath === null || selectedPath === 'report.md')`. This keeps the header decision derived from the same state that chooses the right pane.
2. Smaller callback-only shape: keep `selectedPath` local to `WaveReportSidebar`, add `onSelectedPathChange`, and let `WaveReportCard` track only the last selected path for header gating. Reset that parent value on `waveId`. This touches less sidebar plumbing but splits the source of truth.

## Issue 2 - Cannot create codex card

- The AddPanel menu is populated from registered entries with `addPanel` and a create mode that is not catalog/kernel-only. See `web/src/cards/registry.ts:337`.
- `CodexEntry` opts into AddPanel with label `codex`, a single `cwd` directory field, and atomic create. Its submit path calls `createCodexCard(waveId, { cwd, prompt, theme })`. See `web/src/cards/builtins/codex.tsx:453` and `web/src/cards/builtins/codex.tsx:499`.
- Click path: `AddPanel` maps the menu item to `onSelect(entry)`; `WavePage.beginAdd` opens the schema modal for codex; the sole directory field uses `DirectoryBrowser`, whose `onSelect` calls `submitModal`; `submitModal` calls `onCreateCardWithBody`; router wires that to `addCardWithValues`. See `web/src/shared/components/AddPanel.tsx:64`, `web/src/pages/Wave.tsx:90`, `web/src/pages/Wave.tsx:337`, `web/src/pages/Wave.tsx:357`, `web/src/pages/Wave.tsx:100`, and `web/src/app/router.tsx:381`.
- The frontend endpoint is not `POST /api/cards`; it is `POST /api/waves/{waveId}/codex-cards`. `createFromEntry` reaches `entry.create.submit`, and `createCodexCard` posts that route. See `web/src/app/router.tsx:525` and `web/src/api/calm.ts:317`.
- Failure visibility is weak: the API helper throws `CalmApiError` on non-2xx, but `createFromEntry` catches non-contract create failures and only `console.warn`s; `WavePage.submitModal` closes the modal in `finally`. See `web/src/api/calm.ts:73`, `web/src/api/calm.ts:95`, `web/src/app/router.tsx:535`, and `web/src/pages/Wave.tsx:102`. A 500 can therefore look like a silent no-op except in DevTools/console.
- Backend route: `routes::codex_cards::router` registers `POST /api/waves/{wave_id}/codex-cards`; the route submits a `codex-create` operation and maps failed operations to `CalmError`. See `crates/calm-server/src/routes/codex_cards.rs:50`, `crates/calm-server/src/routes/codex_cards.rs:125`, and `crates/calm-server/src/routes/codex_cards.rs:174`.
- The codex create adapter validates before DB work that the wave exists and `shared_codex_appserver.is_running()`. If the shared server is dead, it returns `CalmError::Internal("shared codex app-server is not running")`; `CalmError::Internal` maps to HTTP 500. See `crates/calm-server/src/operation/codex_adapter.rs:220`, `crates/calm-server/src/operation/codex_adapter.rs:231`, and `crates/calm-server/src/error.rs:155`.
- There is a test for the dead shared-daemon case: it asserts `!is_running()`, posts `/codex-cards`, and expects `500 INTERNAL_SERVER_ERROR`. See `crates/calm-server/tests/codex_user_prompt_shared_daemon.rs:674`.
- Boot failure: `boot_harnesses` attempts `shared_codex_appserver.start_or_takeover()` but logs and continues boot on failure, which matches `docs/_server-log-pr573.txt:5`. See `crates/calm-server/src/lib.rs:696`.
- Spawn path: the shared daemon runs `Command::new(&self.codex_bin).arg("app-server")`; `CALM_CODEX_BIN` defaults to `codex`; Docker bind-mounts `CALM_CODEX_HOST_BIN` into `/usr/local/bin/codex`. See `crates/calm-server/src/shared_codex_appserver.rs:921`, `crates/calm-server/src/config.rs:62`, and `docker-compose.yml:135`.
- Memory note `project_codex_npm_bin_path_change.md` matches this exact class: codex npm moved the host binary from `.../codex/codex` to `.../bin/codex`; the stale default can mount as a directory at `/usr/local/bin/codex`, and spawning it yields `Permission denied (os error 13)`.
- Memory note `project_codex_sandbox_blocks_uds_connect.md` is related to codex shell sandbox UDS access (`connect()` denied as EPERM), but it is not the immediate boot error here. This log is EACCES while spawning the shared app-server binary.

Recommended smallest confirmation:

1. In browser DevTools Network, click `+ Add -> codex -> Create here`. Look specifically for `POST /api/waves/<waveId>/codex-cards`. If present, expected dead-daemon response is HTTP 500 with an `internal: shared codex app-server is not running` body; the UI currently hides that behind a console warning. If absent, the bug is earlier in AddPanel/DirectoryBrowser submit.
2. If the POST returns 500, check the container mount/env: `docker compose exec server sh -lc 'printf "CALM_CODEX_BIN=%s\n" "$CALM_CODEX_BIN"; ls -ld /usr/local/bin/codex'`. If `/usr/local/bin/codex` is a directory or not executable, set `CALM_CODEX_HOST_BIN` to the real npm `.../vendor/x86_64-unknown-linux-musl/bin/codex` path and restart the stack.
