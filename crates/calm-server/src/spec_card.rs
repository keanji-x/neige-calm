//! Spec-card binding (PR6 of #136).
//!
//! Every wave gets a single auto-minted **spec card** at create-time. The
//! spec card is the wave's "AI authority": the only card whose `AiSpec`
//! actor is allowed to emit `Event::WaveUpdated` (per `enforce_role`),
//! and the one whose Codex daemon runs with a system prompt scoped to
//! the wave's goal + acceptance criteria.
//!
//! This module owns the role-specific prompts and shared-remote TUI command
//! construction:
//!
//!   1. [`SPEC_SYSTEM_PROMPT_TEMPLATE`] — the system prompt used when
//!      starting the spec card's Codex thread. PR6 ships a minimal
//!      placeholder; PR7a flips on the kernel-as-MCP-server config
//!      block here.
//!
//! Atomicity story for the spec card itself lives in
//! `routes::waves::create_wave` — the spec card row, its terminal row,
//! and both `Event::WaveUpdated` / `Event::CardAdded` envelopes are
//! produced in a single `write_with_events_typed` transaction. The
//! daemon spawn happens post-commit; on failure the orphan-terminal sweeper
//! reaps the persisted rows (~60s) per the same recovery semantics as
//! `routes::codex_cards::create_codex_card`.

use crate::error::{CalmError, Result};
use crate::routes::codex_cards::shell_single_quote;
use crate::routes::terminal::spawn_terminal_for;
use crate::state::{AppState, CodexClient};

/// Minimal spec-agent system prompt template. PR6 ships a placeholder
/// that documents the role; PR7a/PR7b will expand this with explicit
/// instructions for the `wave_state.update` / `wave_state.get` MCP tools
/// once those land.
///
/// `{wave_id}` is the only substitution: when the Codex thread starts,
/// the kernel replaces it with the freshly minted wave id so the agent has
/// a stable reference for the `calm.*` wave-state / report tools.
///
/// Kept short on purpose: the codex CLI prepends this to every turn, so
/// every additional token is a per-turn cost. The substantive instructions
/// will arrive in the MCP tool descriptors that PR7b registers.
pub(crate) const SPEC_SYSTEM_PROMPT_TEMPLATE: &str = "\
You are the spec agent for wave `{wave_id}`.

You are the wave's sole long-running AI authority and the only actor \
(besides the user) that may drive the wave's lifecycle state machine. \
Worker cards report task results; you decide what state the wave is in.

## Wave lifecycle (issue #145)

Every wave has an explicit `lifecycle` field that you must advance \
through the canonical happy path:

  draft → planning → dispatching → working → reviewing → done

Branches:
  * working → blocked         when you need user input you cannot resolve
  * blocked → working         after the user unblocks (you may also drive this)
  * working → reviewing       when worker results are ready to validate
  * reviewing → working       when more work is needed
  * reviewing → failed        when the wave cannot be completed
  * (only the user may drive cancellation / reopen)

You drive transitions by calling `calm.update_wave_state` with a \
`lifecycle` argument naming the target state. The kernel validates the \
(from → to, actor=spec) edge; an illegal transition is rejected and \
nothing is persisted. Move the wave to `planning` as soon as you read \
the goal, `dispatching` before your first `calm.dispatch_request`, \
`working` once a worker is running, `reviewing` when results land, and \
`done` only after acceptance.

## How you are driven

You are **turn-reactive**, not a polling loop. The kernel re-invokes you \
once per observation, pushed into your context as the input for a new \
turn. Each turn begins with exactly one of:

  * the **wave goal** (your first turn);
  * a **dispatched task completed or failed** (a worker reported \
    `task.completed` / `task.failed` against one of your idempotency keys);
  * the **user edited the wave report** (a `wave.report_edited` from the user).

On each turn:

Read wave state with the `neige` shell CLI (`neige state`, `neige ls`, \
`neige cat`); mutate the wave with the `calm.*` MCP tools. Reads observe; \
writes are transactional.

1. Run `neige state` to read the wave's current shape (lifecycle, \
   dispatched jobs, task results). This is your ground truth — do NOT keep \
   a private model of wave state across turns.
2. Decide what to do next and act:
   * Advance lifecycle via `calm.update_wave_state(lifecycle=...)` — \
     `planning` once you've read the goal, `dispatching` before your \
     first `calm.dispatch_request`, `working` once a worker is running, \
     `reviewing` when results land, `done` only after acceptance, \
     `blocked` when you need the user.
   * Dispatch sub-jobs via `calm.dispatch_request`. Required args: \
     `kind` (\"codex\" or \"terminal\"), `idempotency_key` (stable \
     across retries so a redelivered observation can't double-dispatch), \
     plus `goal` (codex) or `cmd` (terminal).
   * Record verdicts via `calm.update_task_meta(status=...)` when worker \
     output is ready to validate.
   * Keep the wave report current (see below).
3. **END YOUR TURN.** Do NOT poll, do NOT call `calm.wait_for_events` \
   (it no longer exists), do NOT loop waiting for the next event. The \
   kernel pushes the next observation as a fresh turn the moment it \
   arrives — you will be re-invoked automatically. If there is nothing \
   left to do this turn, just stop; if the wave is `done`/`failed`/ \
   `blocked` and you're waiting on the user, stop and wait to be \
   re-invoked.

Update other wave metadata (title, archive status) only when it genuinely \
changes. Worker cards must NOT touch the wave row; the kernel's role gate \
enforces this.

## Wave Report (issue #229)

The wave has a user-facing Markdown report you maintain. The user sees \
it as the top card on the Wave page. Treat it like a file you keep \
updated. READ the current body with `neige cat report.md` (returns the \
report body). WRITE/EDIT via MCP tools that target the wave's report \
instead of a disk path:

  * `calm.report.write(body, summary?)` — wholesale replace (like Write).
  * `calm.report.edit(old_string, new_string, replace_all?)` — string \
    replacement (like Edit; `old_string` must be unique in the body or \
    you must pass `replace_all=true`).

Structure the `body` with H1 headings the UI renders as collapsible \
sections. Canonical headings (use these names so the UI's section \
styling matches):

  * `# Goal` — what the wave is trying to accomplish, in 1–3 sentences.
  * `# Progress` — what's been done so far, terse bullets.
  * `# Needs attention` — anything you're blocked on or want the user \
    to look at. The UI styles this section with a warning border so \
    the user sees it on glance.
  * `# Results` — links / paths / PRs you've produced.
  * `# Timeline` — a chronological log of significant events. The UI \
    collapses this by default.

`summary` is the one-line preview the sidebar / wave-list shows. Keep \
it under ~80 characters.

Update the report whenever:
  * the goal becomes clearer (overwrite `# Goal`);
  * you make material progress (append to `# Progress`);
  * you get blocked or need the user (write into `# Needs attention`);
  * a worker produces an artifact (add to `# Results`).

Do NOT duplicate the lifecycle state in the body — the user already \
sees the lifecycle badge in the card header. Keep the report terse: \
it's a status board, not a chat log.

### Reacting to user edits

The user can edit the report directly from the UI. When that happens, \
the kernel re-invokes you with a `wave.report_edited` (author = \
\"user\") observation as that turn's input. Before doing anything else \
on that turn:

1. Run `neige cat report.md` to fetch the latest body.
2. Reconcile the user's changes with what you were about to write — \
   treat their version as ground truth for the sections they touched.
3. Then continue your task. Do NOT blindly `report.write` your \
   previous draft; that would overwrite the user's edits.

You will never be pushed your own (`author = \"spec\"`) edits — the \
kernel only re-invokes you for user-authored report edits.

## Reading worker outputs (issue #339)

`neige state` deliberately returns metadata only — wave row plus a cards \
list with id/kind/role/sort/created_at/updated_at, **no card payloads, \
no event payloads, no worker results**. To read what a worker actually \
produced, use the read-only wave views from your shell via the `neige` \
CLI, which composes with tools like `grep`, `jq`, and `head`:

  * `neige ls [path]` — directory listing, e.g. `neige ls runs/` or \
    `neige ls /`.
  * `neige cat <path>` — read one view, e.g. `neige cat runs/K.md`, \
    `neige cat runs/index.json`, or \
    `neige cat cards/<card_id>/payload.json`.

Available `<path>` values for `neige cat` / `neige ls`:

  * `runs/<idempotency_key>.md` — human-readable summary of one run \
    (status, worker output, verdict if recorded).
  * `runs/<idempotency_key>.json` — structured projection. \
    `events.completed.payload.result` is the worker's actual output; \
    `events.failed` carries failures; `verdict` holds any \
    `update_task_meta` accept/reject you recorded; `worker_card.payload` \
    has the dispatch context.
  * `runs/index.json` — array of all runs in the wave with status, kind, \
    requested_at, finished_at, worker_card_id, and verdict.
  * `cards/<card_id>/payload.json` — full payload of any card in the \
    wave (e.g. another worker's bookkeeping).
  * `/` — root directory listing.
  * `report.md` — current wave report body.

When you are pushed \"A dispatched task completed \
(idempotency_key=K)...\", the canonical first read is \
`neige cat runs/K.md` to see what the worker did. The push \
observation is just a notification; the result lives in this view, not \
in `neige state`.

The view is READ-ONLY. To act on what you read, call \
`calm.update_task_meta(idempotency_key=K, status=\"accepted\" | \
\"rejected\")` to record a verdict, and/or `calm.dispatch_request` to \
start follow-up work — the same tools as before.

Wave is implicit — derived from your card identity. Do NOT pass a \
`wave_id` (these tools have no such parameter; cross-wave reads are \
forbidden by design).

Do not mint new spec cards from within this session.
";

/// Worker-agent system prompt. PR8 (#136) replaces the PR6 stub with
/// the production prompt: workers are short-lived, fire-and-forget,
/// driven by the spec card via `calm.dispatch_request`. They run one
/// job and exit.
///
/// The name retains the `_PLACEHOLDER` suffix only to avoid churn in
/// downstream call sites; the content is now production. A followup
/// can rename this to `WORKER_SYSTEM_PROMPT_TEMPLATE` for symmetry
/// with [`SPEC_SYSTEM_PROMPT_TEMPLATE`] when there's no other PR
/// touching this file.
pub(crate) const WORKER_SYSTEM_PROMPT_PLACEHOLDER: &str = "\
You are a worker agent under spec card on wave `{wave_id}`.

You were spawned to execute one job. Your contract:

1. Read the goal, context, and acceptance criteria handed to you. \
   Run `neige state` if you need to inspect the wave's shape before \
   starting — but don't poll it; the wave snapshot you receive once is \
   enough.
2. Execute the task. Make tool calls, write files, run commands \
   — whatever the goal requires.
3. When the task is done, report exactly once:
   * On success: `calm.task_completed(idempotency_key, result, artifacts)` \
     where `idempotency_key` echoes the value from your spawning \
     `*.job_requested` event, `result` is opaque agent output \
     (text or structured JSON), and `artifacts` is a list of any \
     file/blob references you produced.
   * On failure: `calm.task_failed(idempotency_key, reason)` with \
     a free-form failure description.
4. Exit. You are short-lived by design — run your single job and stop. \
   The kernel delivers your `task.completed` / `task.failed` to the \
   spec card as a pushed turn input, and the spec continues the wave \
   from there. You do not wait for or observe anything.

You may NOT call `calm.update_wave_state` or `calm.update_task_meta` — \
those are spec-only tools and the kernel's role gate will refuse you. \
You also may NOT mint new workers via `calm.dispatch_request`. If the \
job needs further decomposition, report `task.failed` with a reason \
explaining what's missing and the spec will handle re-decomposition.

## Reading wave state

You may read your wave's state READ-ONLY from the shell with the `neige` \
CLI: `neige state` reads the wave shape, `neige ls [path]` lists views, \
and `neige cat <path>` reads one view. Useful paths include `/`, \
`runs/index.json`, \
`runs/<idempotency_key>.md`, `runs/<idempotency_key>.json`, and \
`cards/<card_id>/payload.json`. These views are own-wave-only; \
cross-wave reads are forbidden.
";

/// Substitute the per-spawn placeholders into a prompt template. Today
/// the only placeholder is `{wave_id}`; lifted out as its own helper so
/// PR7+ can extend the substitution set without rewriting call sites.
pub(crate) fn render_system_prompt(template: &str, wave_id: &str) -> String {
    template.replace("{wave_id}", wave_id)
}

/// Assemble the env map a per-card codex daemon needs:
///   * `CODEX_HOME` — per-card directory; auth, hooks, config.toml.
///   * `NEIGE_CARD_ID` — surfaced to plugins/MCP for write attribution.
///   * `NEIGE_CALM_BASE_URL` — codex hook + MCP ingest URL.
///   * `HTTP(S)_PROXY` / lowercase variants when settings have a non-
///     empty proxy override.
///   * `NEIGE_MCP_TOKEN` — per-card raw MCP token (PR7a). Only set
///     when `mcp_token` is `Some(...)`; the codex daemon's
///     `[mcp_servers.calm].env` block forwards this to the spawned
///     `neige-mcp-stdio-shim` so the shim's `initialize` request
///     embeds it under `_meta["dev.neige/auth"].token`. Plain cards
///     receive `None` here and have no MCP server block in their
///     config.toml.
///   * `NEIGE_MCP_SOCKET` — kernel-as-MCP-server UDS path (PR7a).
///     Same gating as `NEIGE_MCP_TOKEN`: only emitted when MCP is
///     wired up for this card.
///
/// Settings-driven proxy override matches `routes::codex_cards`
/// semantics: only write proxy env when the override is non-empty (an
/// empty override would *clear* the container default, the opposite of
/// user intent). The settings are read at the call site — the helper
/// takes the resolved values so it can stay sync-only.
pub(crate) fn build_codex_env_map(
    codex: &CodexClient,
    card_id: &str,
    http_proxy: Option<&str>,
    https_proxy: Option<&str>,
    mcp_token: Option<&str>,
    mcp_socket_path: Option<&std::path::Path>,
) -> serde_json::Value {
    let codex_home_path = codex.codex_home_dir().to_string_lossy().to_string();
    let mut env_map = serde_json::Map::new();
    env_map.insert(
        "CODEX_HOME".to_string(),
        serde_json::Value::String(codex_home_path),
    );
    env_map.insert(
        "NEIGE_CARD_ID".to_string(),
        serde_json::Value::String(card_id.to_string()),
    );
    env_map.insert(
        "NEIGE_CALM_BASE_URL".to_string(),
        serde_json::Value::String(codex.ingest_url.clone()),
    );
    if let Some(p) = http_proxy.filter(|s| !s.is_empty()) {
        env_map.insert(
            "HTTP_PROXY".to_string(),
            serde_json::Value::String(p.to_string()),
        );
        env_map.insert(
            "http_proxy".to_string(),
            serde_json::Value::String(p.to_string()),
        );
    }
    if let Some(p) = https_proxy.filter(|s| !s.is_empty()) {
        env_map.insert(
            "HTTPS_PROXY".to_string(),
            serde_json::Value::String(p.to_string()),
        );
        env_map.insert(
            "https_proxy".to_string(),
            serde_json::Value::String(p.to_string()),
        );
    }
    // PR7a — wire per-card MCP token + socket path. Both must be
    // `Some` together: a token without a socket is unusable, and a
    // socket without a token can't initialize. The caller threads
    // them in only for Spec/Worker cards (Plain cards don't mint a
    // token row and don't get an MCP server block in config.toml).
    if let (Some(token), Some(socket)) = (mcp_token, mcp_socket_path) {
        env_map.insert(
            "NEIGE_MCP_TOKEN".to_string(),
            serde_json::Value::String(token.to_string()),
        );
        env_map.insert(
            "NEIGE_MCP_SOCKET".to_string(),
            serde_json::Value::String(socket.to_string_lossy().to_string()),
        );
    }
    serde_json::Value::Object(env_map)
}


/// Roles that legitimately need role-specific Codex setup.
/// Carved out of [`crate::model::CardRole`] so the seeding helper can
/// only ever be handed a value that maps to a system-prompt template
/// (no `Plain` arm to silently fall through). PR6 followup of issue
/// #136 — note 3 from the original review.
///
/// `Plain` cards still flow through `routes::codex_cards`'s simpler
/// seed path (which writes a no-prompt config.toml inline); they
/// must not reach this helper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SeededCardRole {
    /// Spec card minted by `routes::waves::create_wave`. Gets
    /// [`SPEC_SYSTEM_PROMPT_TEMPLATE`].
    Spec,
    /// Worker card minted by the dispatcher. Gets
    /// [`WORKER_SYSTEM_PROMPT_PLACEHOLDER`] (PR8 will swap in the
    /// production worker prompt).
    Worker,
}

impl SeededCardRole {
    pub(crate) fn prompt_template(self) -> &'static str {
        match self {
            SeededCardRole::Spec => SPEC_SYSTEM_PROMPT_TEMPLATE,
            SeededCardRole::Worker => WORKER_SYSTEM_PROMPT_PLACEHOLDER,
        }
    }
}


/// PR3a (#293) — push-mode PTY daemon arguments for
/// [`seed_and_spawn_spec_daemon`]. Carries the `app-server` listen socket
/// and, for non-empty goals, the codex thread id the kernel already created
/// and drove turn #1 on. Empty-goal waves intentionally leave the thread id
/// absent so the `--remote` TUI fresh-starts and the kernel observes the
/// first `turn/started`.
#[derive(Debug, Clone)]
pub(crate) struct SpecPushDaemonArgs {
    /// `codex_thread_id` — the shared thread; `codex resume <thread_id>`.
    /// `None` means empty-goal fresh start: `codex --remote <sock>`, with
    /// an optional `-c developer_instructions=...` override.
    pub thread_id: Option<String>,
    /// The app-server remote URI; `--remote unix://<sock>`.
    pub sock_uri: String,
    /// Optional role prompt for remote fresh-starts that do not use a
    /// per-card CODEX_HOME. Passed via `codex -c developer_instructions=...`.
    pub developer_instructions: Option<String>,
}

impl SpecPushDaemonArgs {
    /// Build the PTY daemon command line for push mode. Non-empty goals use
    /// `codex resume <thread_id> --remote unix://<sock>` byte-for-byte as
    /// before. Empty goals use `codex --remote unix://<sock>` so the TUI
    /// fresh-starts instead of trying to resume a rollout-less thread.
    pub(crate) fn command_line(&self) -> String {
        match &self.thread_id {
            Some(thread_id) => format!(
                "codex resume {} --remote {}",
                shell_single_quote(thread_id),
                shell_single_quote(&self.sock_uri),
            ),
            None => {
                if let Some(developer_instructions) = self.developer_instructions.as_deref() {
                    format!(
                        "codex -c {} --remote {}",
                        shell_single_quote(&developer_instructions_config_override(
                            developer_instructions
                        )),
                        shell_single_quote(&self.sock_uri)
                    )
                } else {
                    format!("codex --remote {}", shell_single_quote(&self.sock_uri))
                }
            }
        }
    }
}

fn developer_instructions_config_override(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n");
    format!("developer_instructions=\"{escaped}\"")
}


/// Spawn the spec card's shared-remote TUI daemon bound to its terminal row.
///
/// Issue #236 (closes): this used to be invoked from `tokio::spawn`
/// off the response hot path. That opened a TOCTOU race against
/// `ws::terminal::resolve_live_renderer` (frontend WS attach between
/// commit and background task → respawn from baked terminal-row env
/// missing MCP vars → two daemons racing on one socket). The
/// `create_wave` handler now awaits this inline; the return type is
/// `Result<()>` so the route can surface a 5xx on failure rather
/// than silently dropping the spawn.
///
/// On error: the persisted rows (wave + spec card + terminal) survive
/// in the DB regardless. The route maps the error to a 500 so the
/// client knows the wave is unusable and may retry; the
/// orphan-terminal sweeper reaps the dangling terminal row within
/// ~60 s if the user abandons it. Returning `Ok(())` on the spawn
/// failure paths would let `create_wave` return 201 even though no
/// daemon is running, which is exactly the kind of "looks fine but
/// isn't" failure mode #236 was about.
///
/// Inputs are owned (`String` / `CardId` / `WaveId` / `serde_json::Value`)
/// for back-compat with prior `tokio::spawn` callsites; the
/// `'static`-future cost is one clone of each at the route boundary.
///
// The create-wave path threads several owned inputs through to the
// post-commit spawn (state, ids, cwd, env, token, push args). Bundling them
// buys nothing here (each is used once, at the single route call site) and
// the codebase already uses this allow for the same input-threading shape
// (see `dispatcher.rs`, `db/sqlite.rs`).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn seed_and_spawn_spec_daemon(
    state: AppState,
    spec_card_id: String,
    wave_id: String,
    cwd: String,
    env: serde_json::Value,
    _mcp_token: Option<String>,
    // #293/#419 — push-mode arguments. Non-empty goals use the original
    // resume path: `create_wave` has already booted the kernel-owned
    // `codex app-server`, run turn #1, and persisted the thread id; the
    // PTY daemon runs `codex resume <thread_id> --remote unix://<sock>` to
    // rejoin the kernel's thread. Empty goals use `codex --remote
    // unix://<sock>`; the TUI fresh-starts and the kernel backfills the
    // thread id after observing the first turn lifecycle.
    push: SpecPushDaemonArgs,
) -> Result<()> {
    // 1. Look up the terminal row. Guaranteed to exist post-commit
    //    (the row was written inside the same tx as the spec card).
    let term = match state.repo.terminal_get_by_card(&spec_card_id).await {
        Ok(Some(t)) => t,
        Ok(None) => {
            tracing::warn!(
                card_id = %spec_card_id,
                wave_id = %wave_id,
                "spec terminal row missing after commit; orphan terminal will be reaped by sweeper",
            );
            return Err(CalmError::Internal(format!(
                "spec terminal row missing for card {spec_card_id}",
            )));
        }
        Err(e) => {
            tracing::warn!(
                card_id = %spec_card_id,
                wave_id = %wave_id,
                error = %e,
                "spec terminal lookup failed; orphan terminal will be reaped by sweeper",
            );
            return Err(e);
        }
    };

    // 2. Spawn the daemon. For non-empty goals, the spec agent's prompt was
    //    already sent through the kernel-owned app-server thread/start call.
    //    For empty goals, the TUI fresh-start path creates the thread using
    //    the developer_instructions baked into this card's config.toml, and
    //    the kernel observes the resulting lifecycle.
    //
    //    #293/#419 — push is the only path. Non-empty goals still resume
    //    the thread the kernel already created and started turn #1 on.
    //    Empty goals omit `resume` and let the TUI create the thread, while
    //    queued spec pushes wait for the observed thread id. Shared-home
    //    empty goals pass their role prompt through `-c
    //    developer_instructions=...`.
    //
    //    `spawn_terminal_for` waits for deterministic daemon readiness on the
    //    response hot path. Since #236, that synchronous wait is intentional:
    //    it is the acceptable cost vs. the correctness bug it closes.
    let command_line = push.command_line();
    if let Err(e) = spawn_terminal_for(&state, &term, &command_line, &cwd, &env).await {
        tracing::warn!(
            card_id = %spec_card_id,
            wave_id = %wave_id,
            error = %e,
            "spec card daemon spawn failed; orphan terminal will be reaped by sweeper",
        );
        return Err(e);
    }

    tracing::info!(
        card_id = %spec_card_id,
        wave_id = %wave_id,
        terminal_id = %term.id,
        "spec card + daemon spawned for new wave (synchronous on create)",
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PR3a (#293) — push-mode command is `codex resume <tid> --remote
    /// unix://<sock>`, with both the thread id and the `unix://` URI
    /// shell-quoted (the command runs under `sh -c`).
    #[test]
    fn push_mode_command_line_is_codex_resume_remote() {
        let args = SpecPushDaemonArgs {
            thread_id: Some("thread-abc123".into()),
            sock_uri: "unix:///home/u/.local/share/neige-calm/appserver/card-9/app.sock".into(),
            developer_instructions: None,
        };
        assert_eq!(
            args.command_line(),
            "codex resume 'thread-abc123' \
             --remote 'unix:///home/u/.local/share/neige-calm/appserver/card-9/app.sock'",
        );
    }

    /// Shell metacharacters in the thread id / socket path must land in
    /// codex's argv verbatim (single-quoted), not be interpreted by the
    /// `sh -c` wrapper.
    #[test]
    fn push_mode_command_line_quotes_metacharacters() {
        let args = SpecPushDaemonArgs {
            thread_id: Some("a b; rm -rf /".into()),
            sock_uri: "unix:///tmp/has space/app.sock".into(),
            developer_instructions: None,
        };
        let line = args.command_line();
        assert!(
            line.starts_with(
                "codex resume 'a b; rm -rf /' --remote 'unix:///tmp/has space/app.sock'"
            ),
            "metacharacters must be single-quoted; got: {line}"
        );
    }

    #[test]
    fn empty_goal_command_line_is_codex_remote_fresh_start() {
        let args = SpecPushDaemonArgs {
            thread_id: None,
            sock_uri: "unix:///tmp/has space/app.sock".into(),
            developer_instructions: None,
        };
        assert_eq!(
            args.command_line(),
            "codex --remote 'unix:///tmp/has space/app.sock'",
        );
    }

    #[test]
    fn empty_goal_command_line_can_pass_developer_instructions_config_override() {
        let args = SpecPushDaemonArgs {
            thread_id: None,
            sock_uri: "unix:///tmp/spec/app.sock".into(),
            developer_instructions: Some("line 1\nsay \"hi\" and don't expand $HOME".into()),
        };
        assert_eq!(
            args.command_line(),
            "codex -c 'developer_instructions=\"line 1\\nsay \\\"hi\\\" and don'\\''t expand $HOME\"' \
             --remote 'unix:///tmp/spec/app.sock'",
        );
    }

    #[test]
    fn render_system_prompt_substitutes_wave_id() {
        let out = render_system_prompt(SPEC_SYSTEM_PROMPT_TEMPLATE, "wave-abc");
        assert!(
            out.contains("wave `wave-abc`"),
            "wave id should be substituted; got: {out}"
        );
        assert!(
            !out.contains("{wave_id}"),
            "placeholder should be gone; got: {out}"
        );
    }

    #[test]
    fn render_system_prompt_preserves_role_template_content() {
        let spec = render_system_prompt(SeededCardRole::Spec.prompt_template(), "wave-abc");
        assert!(spec.contains("You are the spec agent for wave `wave-abc`."));
        assert!(spec.contains("calm.update_wave_state"));

        let worker = render_system_prompt(SeededCardRole::Worker.prompt_template(), "wave-abc");
        assert!(worker.contains("You are a worker agent under spec card on wave `wave-abc`."));
        assert!(worker.contains("calm.task_completed"));
    }

    /// #293 cutover — the spec prompt must be push-native, not pull. It must
    /// NOT instruct the agent to poll via `calm.wait_for_events`, and it must
    /// carry the turn-reactive guidance (driven by pushed observations, end
    /// the turn, no looping). The only allowed mention of `wait_for_events`
    /// is the explicit "do NOT call it" instruction.
    #[test]
    fn spec_prompt_is_push_native_not_pull() {
        let p = SPEC_SYSTEM_PROMPT_TEMPLATE;

        // No pull loop. The single permitted occurrence of the old tool
        // name is the explicit prohibition; it must never be presented as
        // a thing to call (e.g. `calm.wait_for_events(...)` with args).
        assert!(
            !p.contains("calm.wait_for_events(timeout_ms"),
            "prompt must not tell the spec to poll wait_for_events with a timeout"
        );
        assert!(
            !p.contains("long-poll"),
            "prompt must not describe a long-poll loop"
        );
        // The one mention that remains is the "do NOT call" guidance.
        assert!(
            p.contains("do NOT call `calm.wait_for_events`"),
            "prompt should explicitly tell the agent wait_for_events is gone"
        );

        // Turn-reactive guidance present.
        assert!(
            p.contains("turn-reactive") || p.contains("END YOUR TURN"),
            "prompt must carry turn-reactive guidance"
        );
        assert!(
            p.contains("END YOUR TURN"),
            "prompt must tell the agent to end its turn"
        );
        assert!(
            p.contains("re-invoked"),
            "prompt must explain the kernel re-invokes the agent per observation"
        );
        assert!(
            p.contains("Do NOT poll") && p.contains("do NOT loop"),
            "prompt must forbid polling / looping"
        );
        // Reads go through the shell CLI; writes still go through MCP.
        assert!(
            p.contains("Run `neige state`") && p.contains("calm.dispatch_request"),
            "prompt must read state via neige and still dispatch via MCP"
        );
        assert!(
            p.contains("calm.update_wave_state")
                && p.contains("calm.dispatch_request")
                && p.contains("calm.update_task_meta"),
            "prompt must still document wave/task write tools"
        );
        assert!(
            !p.contains("Call `calm.get_wave_state`"),
            "prompt must not instruct state reads via MCP"
        );
    }

    #[test]
    fn spec_prompt_documents_neige_reads_for_worker_outputs() {
        let p = SPEC_SYSTEM_PROMPT_TEMPLATE;

        assert!(
            p.contains("neige state") && p.contains("neige cat") && p.contains("neige ls"),
            "spec prompt must document the shell neige read CLI"
        );
        assert!(
            p.contains("neige cat report.md"),
            "spec prompt must document reading the report through neige"
        );
        assert!(
            p.contains("runs/<idempotency_key>"),
            "spec prompt must document run projections by idempotency key"
        );
        assert!(
            p.contains("READ-ONLY"),
            "spec prompt must state wave file views are read-only"
        );
        assert!(
            p.contains("runs/K.md"),
            "spec prompt must document the canonical post-completion read"
        );
        assert!(
            p.contains("calm.report.write") && p.contains("calm.report.edit"),
            "spec prompt must document report write/edit MCP tools"
        );
        assert!(
            !p.contains("calm.wave.cat")
                && !p.contains("calm.wave.ls")
                && !p.contains("calm.report.read"),
            "spec prompt must not instruct reads via MCP"
        );
    }

    #[test]
    fn worker_prompt_documents_neige_read_cli() {
        let p = WORKER_SYSTEM_PROMPT_PLACEHOLDER;

        assert!(
            p.contains("neige state") && p.contains("neige cat") && p.contains("neige ls"),
            "worker prompt must document the shell neige read CLI"
        );
        assert!(
            p.contains("calm.task_completed") && p.contains("calm.task_failed"),
            "worker prompt must still document task completion writes"
        );
        assert!(
            p.contains("READ-ONLY") && p.contains("own-wave-only"),
            "worker prompt must constrain neige reads to read-only own-wave views"
        );
    }

}
