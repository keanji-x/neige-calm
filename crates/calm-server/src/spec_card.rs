//! Spec-card binding (PR6 of #136).
//!
//! Every wave gets a single auto-minted **spec card** at create-time. The
//! spec card is the wave's "AI authority": the only card whose `AiSpec`
//! actor is allowed to emit `Event::WaveUpdated` (per `enforce_role`),
//! and the one whose Codex daemon runs with a system prompt scoped to
//! the wave's goal + acceptance criteria.
//!
//! This module owns two things:
//!
//!   1. [`SPEC_SYSTEM_PROMPT_TEMPLATE`] — the system prompt baked into the
//!      spec card's `$CODEX_HOME/config.toml`. PR6 ships a minimal
//!      placeholder; PR7a flips on the kernel-as-MCP-server config
//!      block here.
//!   2. [`seed_codex_home_for_card`] — a reusable helper that mirrors
//!      what `routes::codex_cards` does (mkdir `$CODEX_HOME`, optional
//!      host seed, write `config.toml`), but with a
//!      [`crate::model::CardRole`] discriminator so spec cards get the
//!      spec system prompt and worker cards get their own template
//!      (PR8 wires the worker prompt; PR6 leaves it as a stub). Hooks
//!      come from `/etc/codex/requirements.toml` (policy-managed) so no
//!      per-card `hooks.json` is written here.
//!
//! Atomicity story for the spec card itself lives in
//! `routes::waves::create_wave` — the spec card row, its terminal row,
//! and both `Event::WaveUpdated` / `Event::CardAdded` envelopes are
//! produced in a single `write_with_events_typed` transaction. The
//! daemon spawn + filesystem seeding happen post-commit; on failure the
//! orphan-terminal sweeper reaps the persisted rows (~60s) per the same
//! recovery semantics as `routes::codex_cards::create_codex_card`.

use std::path::PathBuf;

use crate::error::{CalmError, Result};
use crate::routes::codex_cards::{copy_dir_recursive, host_codex_dir, shell_single_quote};
use crate::routes::terminal::spawn_daemon_for;
use crate::state::{AppState, CodexClient};

/// Minimal spec-agent system prompt template. PR6 ships a placeholder
/// that documents the role; PR7a/PR7b will expand this with explicit
/// instructions for the `wave_state.update` / `wave_state.get` MCP tools
/// once those land.
///
/// `{wave_id}` is the only substitution: at seed time the kernel replaces
/// it with the freshly minted wave id so the agent has a stable reference
/// for the `calm.*` wave-state / report tools.
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

1. Call `calm.get_wave_state` to read the wave's current shape (lifecycle, \
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
updated — the kernel exposes three MCP tools that behave exactly like \
your native Read / Edit / Write file tools, but target the wave's \
report instead of a disk path:

  * `calm.report.read()` — fetch the current `{ body, summary, schemaVersion, updated_at }`.
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

1. Call `calm.report.read` to fetch the latest body.
2. Reconcile the user's changes with what you were about to write — \
   treat their version as ground truth for the sections they touched.
3. Then continue your task. Do NOT blindly `report.write` your \
   previous draft; that would overwrite the user's edits.

You will never be pushed your own (`author = \"spec\"`) edits — the \
kernel only re-invokes you for user-authored report edits.

## Reading worker outputs (issue #339)

`get_wave_state` deliberately returns metadata only — wave row plus a \
cards list with id/kind/role/sort/created_at/updated_at, **no card \
payloads, no event payloads, no worker results**. To read what a worker \
actually produced, use the file-like `calm.wave.cat` tool. It exposes \
the current wave as read-only paths:

  * `calm.wave.cat path=\"runs/<idempotency_key>.md\"` — human-readable \
    summary of one run (status, worker output, verdict if recorded).
  * `calm.wave.cat path=\"runs/<idempotency_key>.json\"` — structured \
    projection. `events.completed.payload.result` is the worker's actual \
    output; `events.failed` carries failures; `verdict` holds any \
    `update_task_meta` accept/reject you recorded; `worker_card.payload` \
    has the dispatch context.
  * `calm.wave.cat path=\"cards/<card_id>/payload.json\"` — full payload \
    of any card in the wave (e.g. another worker's bookkeeping).
  * `calm.wave.cat path=\"runs/index.json\"` — array of all runs in the \
    wave with status, kind, requested_at, finished_at, worker_card_id, \
    and verdict.
  * `calm.wave.ls path=\"/\"` — directory listing. `calm.wave.ls \
    path=\"runs/\"` lists run entries; `calm.wave.ls path=\"cards/\"` \
    lists cards.

The same read-only views are available from your shell via the `neige` \
CLI, which composes with tools like `grep`, `jq`, and `head`:

  * `neige ls [path]` — directory listing, e.g. `neige ls runs/` or \
    `neige ls /`.
  * `neige cat <path>` — read one view, e.g. `neige cat runs/K.md`, \
    `neige cat runs/index.json`, or \
    `neige cat cards/<card_id>/payload.json`.

The paths are identical to the MCP-tool `path` arguments. The views are \
READ-ONLY, and the MCP-tool form remains available as the in-band \
equivalent.

When you are pushed \"A dispatched task completed \
(idempotency_key=K)...\", the canonical first call is \
`calm.wave.cat path=\"runs/K.md\"` to see what the worker did. The push \
observation is just a notification; the result lives in this view, not \
in `get_wave_state`.

The view is READ-ONLY. To act on what you read, call \
`update_task_meta(idempotency_key=K, status=\"accepted\" | \"rejected\")` \
to record a verdict, and/or `dispatch_request` to start follow-up work \
— the same tools as before.

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
   Call `calm.get_wave_state()` if you need to inspect the wave's \
   shape before starting — but don't poll it; the wave snapshot \
   you receive once is enough.
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
CLI: `neige ls [path]` lists views and `neige cat <path>` reads one \
view. Useful paths include `/`, `runs/index.json`, \
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
    let codex_home = codex.codex_homes_dir.join(card_id);
    let codex_home_path = codex_home.to_string_lossy().to_string();
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

/// Per-spawn `$CODEX_HOME/config.toml` body, role-aware.
///
/// Same shape as `routes::codex_cards::build_codex_config_toml` but with
/// three additions:
///   * an `instructions = "<system_prompt>"` field — codex CLI reads
///     `~/.codex/config.toml` for `instructions` to prepend to every
///     turn; baking it here keeps spec/worker agents agent-typed
///     without an out-of-band registry.
///   * `[mcp_servers.calm]` + `[mcp_servers.calm.env]` (PR7a, #236
///     followup) — points codex at the `neige-mcp-stdio-shim` binary
///     which bridges stdio JSON-RPC to the kernel's UDS, and bakes
///     `NEIGE_MCP_SOCKET` / `NEIGE_MCP_TOKEN` directly into the
///     subprocess env. Pre-followup we relied on codex inheriting
///     these from the daemon's env, but empirically (codex CLI 0.132)
///     codex spawns MCP server subprocesses with a clean env: the
///     shim would exit immediately with `missing NEIGE_MCP_SOCKET`.
///     Baking the env into the toml block bypasses that boundary.
///     Omitted entirely when `mcp_block` is `None` (Plain cards
///     still hit `routes::codex_cards::build_codex_config_toml`
///     which has no MCP block).
///   * Plain `[projects."<cwd>"] trust_level = "trusted"` matches the
///     Plain helper.
///
/// `mcp_block` pairs the shim config with the per-card raw MCP token:
/// both are required together (a token without a socket is unusable,
/// a socket without a token can't authenticate), so we take them as
/// one `Option<(&McpShimConfig, &str)>` rather than two independent
/// `Option`s. The single-option shape also forbids the only mis-paired
/// state ("shim set, token forgotten") at the type level.
///
/// Plain cards (the user-facing `POST /codex-cards` route) keep using
/// `routes::codex_cards::build_codex_config_toml` and pass no
/// `system_prompt`; this helper handles the role-typed paths.
pub(crate) fn build_codex_config_toml_with_prompt(
    cwd: &str,
    system_prompt: &str,
    mcp_block: Option<(&crate::mcp_server::McpShimConfig, &str)>,
) -> String {
    // Hand-written TOML (no `toml` crate in the workspace). Both `cwd`
    // and `system_prompt` need their `"` / `\` escaped for basic-string
    // safety; codex's TOML parser otherwise rejects the file at boot
    // and the daemon spawn fails opaquely.
    let escaped_cwd = cwd.replace('\\', "\\\\").replace('"', "\\\"");
    let escaped_prompt = system_prompt.replace('\\', "\\\\").replace('"', "\\\"");
    // We use a TOML basic string (one line). Newlines in the prompt are
    // escaped to `\n` so the file stays well-formed without resorting
    // to multiline literals (which would require a different escape
    // scheme for embedded `"""`).
    let one_line_prompt = escaped_prompt.replace('\n', "\\n");

    let mut out = format!(
        "# Generated by neige-calm per-spawn — silences codex's first-run\n\
         # dialogs so an auto-submitted \\r lands on the composer.\n\
         approval_policy = \"never\"\n\
         sandbox_mode = \"workspace-write\"\n\
         instructions = \"{one_line_prompt}\"\n\
         \n\
         [projects.\"{escaped_cwd}\"]\n\
         trust_level = \"trusted\"\n"
    );

    if let Some((shim, token)) = mcp_block {
        // PR7a + #236 followup — emit `[mcp_servers.calm]` plus an
        // explicit `[mcp_servers.calm.env]` table. Codex's MCP client
        // spec:
        //   * `command` = absolute path to the shim binary.
        //   * `args` = optional argv tail (we ship empty — the shim
        //     reads the socket from the env).
        //   * `env` table = exact env the shim subprocess sees. We
        //     used to omit this and rely on the codex daemon's own
        //     env being inherited; codex CLI 0.132 spawns MCP server
        //     subprocesses with a clean env, so the shim's
        //     `missing NEIGE_MCP_SOCKET` exit was the symptom. Baking
        //     both vars here bypasses the inheritance boundary
        //     entirely.
        let escaped_shim = shim
            .shim_bin
            .to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        let escaped_socket = shim
            .socket_path
            .to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        // The token is a base64url-ish opaque string today (see
        // `mcp_server::auth::mint_token`), but escape defensively
        // anyway in case the format ever picks up a `"` or `\`.
        let escaped_token = token.replace('\\', "\\\\").replace('"', "\\\"");
        out.push_str(&format!(
            "\n\
             [mcp_servers.calm]\n\
             command = \"{escaped_shim}\"\n\
             args = []\n\
             \n\
             [mcp_servers.calm.env]\n\
             NEIGE_MCP_SOCKET = \"{escaped_socket}\"\n\
             NEIGE_MCP_TOKEN = \"{escaped_token}\"\n"
        ));
        // Codex does not pass the daemon's NEIGE_MCP_* into exec
        // shells (the mcp_servers.calm.env table above only feeds the
        // MCP server subprocess); depending on ignore_default_excludes,
        // it may also drop `*TOKEN*`. Force-inject both vars after all
        // inherit/exclude filtering so shell `neige` can reach the kernel.
        out.push_str(&format!(
            "\n\
             [shell_environment_policy.set]\n\
             NEIGE_MCP_SOCKET = \"{escaped_socket}\"\n\
             NEIGE_MCP_TOKEN = \"{escaped_token}\"\n"
        ));
    }

    out
}

/// Roles that legitimately need a system-prompt-seeded `$CODEX_HOME`.
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
    fn prompt_template(self) -> &'static str {
        match self {
            SeededCardRole::Spec => SPEC_SYSTEM_PROMPT_TEMPLATE,
            SeededCardRole::Worker => WORKER_SYSTEM_PROMPT_PLACEHOLDER,
        }
    }
}

/// Reusable codex `$CODEX_HOME` seeding helper. Carved out of
/// `routes::codex_cards::create_codex_card` so the wave-create path
/// (PR6 Component A) and the dispatcher's worker-card spawn (PR6
/// Component B) share one seeding recipe.
///
/// The recipe is identical to what the plain user-create route did
/// pre-PR6:
///   1. `mkdir -p` the per-card CODEX_HOME under
///      `<codex_homes_dir>/<card_id>/`.
///   2. On a fresh dir, best-effort recursive copy from
///      `$HOME/.codex/` (so the spec/worker session reuses the
///      operator's auth.json and any preconfigured MCP servers).
///   3. Write `config.toml` with the role-typed system prompt and
///      project trust.
///
/// Hooks are NOT seeded here — they come from
/// `/etc/codex/requirements.toml` (bind-mounted, policy-managed). Per-card
/// `$CODEX_HOME/hooks.json` would be treated as untrusted by codex and
/// would re-arm the "Hooks need review" startup modal.
///
/// Returns the resolved `CODEX_HOME` path so the caller can build the
/// env map (`CODEX_HOME = <path>`) for `spawn_daemon_for`.
///
/// `wave_id` is threaded in so the system prompt substitution can
/// reference the wave the card is bound to.
///
/// `mcp_token` is the per-card raw MCP token minted at card-create
/// time (returned from `card_with_codex_create_tx`). When MCP is
/// wired up on the AppState **and** the token is `Some`, the per-card
/// `config.toml` gets a `[mcp_servers.calm].env` block baking the
/// token + socket directly into the shim subprocess env (issue #236
/// followup — codex CLI 0.132 doesn't inherit the daemon env into MCP
/// server subprocesses, so the env must live in the config.toml).
///
/// Only [`SeededCardRole`] values are accepted — Plain cards must
/// route through `routes::codex_cards` instead.
pub(crate) fn seed_codex_home_for_card(
    s: &AppState,
    card_id: &str,
    cwd: &str,
    wave_id: &str,
    role: SeededCardRole,
    mcp_token: Option<&str>,
) -> Result<PathBuf> {
    let shim = s.mcp_server.as_ref().map(|m| m.shim_config.clone());
    // Pair shim + token: only emit the `[mcp_servers.calm]` block
    // when *both* are present. Missing either side leaves the
    // config.toml MCP-less (codex won't try to start the shim).
    let mcp_block = match (shim.as_ref(), mcp_token) {
        (Some(s), Some(t)) => Some((s, t)),
        _ => None,
    };
    seed_codex_home_with_parts(s.codex.as_ref(), card_id, cwd, wave_id, role, mcp_block)
}

/// PR6 (#136) — lower-level seam over [`seed_codex_home_for_card`] that
/// takes a [`CodexClient`] directly. Used by the dispatcher, which
/// doesn't own an `AppState`.
///
/// `mcp_block` carries the shim config + per-card raw MCP token as a
/// single pair: `Some((&shim_config, &raw_token))` for production
/// callers that boot the kernel-as-MCP-server (`AppState::new`) and
/// have a token in scope (every Spec/Worker card_with_codex_create_tx
/// mints one); `None` for test paths that don't boot the MCP server
/// (`from_parts`). When `Some`, the per-card config.toml gets a
/// matching `[mcp_servers.calm]` block including an `env` table that
/// bakes `NEIGE_MCP_SOCKET` / `NEIGE_MCP_TOKEN` into the shim
/// subprocess env (#236 followup — see `build_codex_config_toml_with_prompt`).
pub(crate) fn seed_codex_home_with_parts(
    codex: &CodexClient,
    card_id: &str,
    cwd: &str,
    wave_id: &str,
    role: SeededCardRole,
    mcp_block: Option<(&crate::mcp_server::McpShimConfig, &str)>,
) -> Result<PathBuf> {
    let codex_home = codex.codex_homes_dir.join(card_id);
    let is_fresh = !codex_home.exists();
    std::fs::create_dir_all(&codex_home).map_err(|e| {
        CalmError::Internal(format!("mkdir codex_home {}: {e}", codex_home.display()))
    })?;
    if is_fresh
        && let Some(src) = host_codex_dir()
        && src.exists()
        && let Err(e) = copy_dir_recursive(&src, &codex_home)
    {
        tracing::warn!(error = %e, src = %src.display(), "codex seed copy failed; continuing without it");
    }

    // Hooks come from `/etc/codex/requirements.toml` (policy-managed,
    // bind-mounted via docker-compose). No per-card hooks.json — codex
    // would treat that as untrusted and re-arm the trust modal.

    // config.toml — role-typed. Spec and Worker cards bake the system
    // prompt directly into the file (codex reads `instructions` at
    // launch). Plain cards are unrepresentable at this seam by
    // construction (see [`SeededCardRole`]).
    let system_prompt = render_system_prompt(role.prompt_template(), wave_id);
    let cfg_text = build_codex_config_toml_with_prompt(cwd, &system_prompt, mcp_block);
    let cfg_path = codex_home.join("config.toml");
    std::fs::write(&cfg_path, cfg_text)
        .map_err(|e| CalmError::Internal(format!("write config.toml: {e}")))?;

    Ok(codex_home)
}

/// PR3a (#293) — push-mode PTY daemon arguments for
/// [`seed_and_spawn_spec_daemon`]. Carries the codex thread id the kernel
/// already created + drove turn #1 on, and the `app-server` listen socket
/// the `--remote` TUI rejoins. Built by `create_wave` from the
/// [`crate::spec_appserver::SpecPushHandle`].
#[derive(Debug, Clone)]
pub(crate) struct SpecPushDaemonArgs {
    /// `codex_thread_id` — the shared thread; `codex resume <thread_id>`.
    pub thread_id: String,
    /// The `app-server` listen socket; `--remote unix://<sock>`.
    pub sock: PathBuf,
}

impl SpecPushDaemonArgs {
    /// Build the PTY daemon command line for push mode:
    /// `codex resume <thread_id> --remote unix://<sock>`, with both the
    /// thread id and the socket path shell-quoted (the command is handed
    /// to `sh -c`, so any metacharacters must land in codex's argv
    /// verbatim — same contract as the legacy `codex '<title>'` build).
    fn command_line(&self) -> String {
        let remote = format!("unix://{}", self.sock.display());
        format!(
            "codex resume {} --remote {}",
            shell_single_quote(&self.thread_id),
            shell_single_quote(&remote),
        )
    }
}

/// Seed `$CODEX_HOME` for the spec card, then spawn the codex daemon
/// bound to its terminal row.
///
/// Issue #236 (closes): this used to be invoked from `tokio::spawn`
/// off the response hot path. That opened a TOCTOU race against
/// `ws::terminal::resolve_live_sock` (frontend WS attach between
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
/// `mcp_token` is the per-card raw MCP token freshly minted inside
/// the `create_wave` transaction (returned from
/// `card_with_codex_create_tx`). It's `Option` only because tests
/// using `AppState::from_parts` without an MCP server still flow
/// through this path; in production it's always `Some` for Spec
/// cards. The token is threaded down into the per-card config.toml's
/// `[mcp_servers.calm].env` block (#236 followup) so the shim
/// subprocess sees it even though codex CLI 0.132 doesn't pass the
/// daemon env through.
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
    mcp_token: Option<String>,
    // #293 — push-mode arguments. `create_wave` has already booted the
    // kernel-owned `codex app-server`, run turn #1, and persisted the thread
    // id; the PTY daemon runs `codex resume <thread_id> --remote
    // unix://<sock>` to *rejoin* the kernel's thread (sharing it with the
    // kernel's programmatic client). Push is the only path — there is no
    // legacy bare-`codex '<title>'` fallback.
    push: SpecPushDaemonArgs,
) -> Result<()> {
    // 1. Seed `$CODEX_HOME` for the spec card. Filesystem-only — fast,
    //    bounded by a handful of mkdir + small write_alls.
    if let Err(e) = seed_codex_home_for_card(
        &state,
        &spec_card_id,
        &cwd,
        &wave_id,
        SeededCardRole::Spec,
        mcp_token.as_deref(),
    ) {
        tracing::warn!(
            card_id = %spec_card_id,
            wave_id = %wave_id,
            error = %e,
            "spec card CODEX_HOME seed failed; orphan terminal will be reaped by sweeper",
        );
        return Err(e);
    }

    // 2. Look up the terminal row. Guaranteed to exist post-commit
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

    // 3. Spawn the daemon. The spec agent's system prompt lives in
    //    `$CODEX_HOME/config.toml`'s `instructions` field (seeded above).
    //
    //    #293 — push is the only path: the TUI runs `codex resume
    //    <thread_id> --remote unix://<sock>` to rejoin the thread the kernel
    //    already created and started turn #1 on. The wave goal was already
    //    submitted by the kernel's `turn/start`, so there is no positional
    //    `[PROMPT]` arg and `codex_auto_submit` is skipped on the
    //    `codex_thread_id` payload (no `\r` is injected into the resumed TUI).
    //
    //    `spawn_daemon_for` waits for deterministic daemon readiness on the
    //    response hot path. Since #236, that synchronous wait is intentional:
    //    it is the acceptable cost vs. the correctness bug it closes.
    let command_line = push.command_line();
    if let Err(e) = spawn_daemon_for(&state, &term, &command_line, &cwd, &env).await {
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
            thread_id: "thread-abc123".into(),
            sock: PathBuf::from("/home/u/.local/share/neige-calm/appserver/card-9/app.sock"),
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
            thread_id: "a b; rm -rf /".into(),
            sock: PathBuf::from("/tmp/has space/app.sock"),
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
        // Still references the kernel MCP tools the agent drives each turn.
        assert!(
            p.contains("calm.get_wave_state") && p.contains("calm.dispatch_request"),
            "prompt must still reference get_wave_state + dispatch_request"
        );
    }

    #[test]
    fn spec_prompt_documents_wave_cat_for_worker_outputs() {
        let p = SPEC_SYSTEM_PROMPT_TEMPLATE;

        assert!(
            p.contains("calm.wave.cat"),
            "spec prompt must document calm.wave.cat for reading worker outputs"
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
            p.contains("neige cat") && p.contains("neige ls"),
            "spec prompt must document the shell neige read CLI"
        );
    }

    #[test]
    fn worker_prompt_documents_neige_read_cli() {
        let p = WORKER_SYSTEM_PROMPT_PLACEHOLDER;

        assert!(
            p.contains("neige cat") && p.contains("neige ls"),
            "worker prompt must document the shell neige read CLI"
        );
        assert!(
            p.contains("READ-ONLY") && p.contains("own-wave-only"),
            "worker prompt must constrain neige reads to read-only own-wave views"
        );
    }

    /// PR6 baseline: `mcp_shim = None` produces no `[mcp_servers]`
    /// block. PR7a (#136) adds the positive case below; the negative
    /// case here is now a `mcp_shim=None` regression guard rather
    /// than a "no MCP block ever exists" statement.
    #[test]
    fn role_config_toml_has_no_mcp_servers_block_when_shim_absent() {
        let s = build_codex_config_toml_with_prompt("/workspace", "you are a spec agent.", None);
        assert!(
            !s.contains("[mcp_servers"),
            "role-typed config.toml must not contain mcp_servers blocks when mcp_shim is None; got:\n{s}"
        );
        assert!(
            !s.contains("shell_environment_policy"),
            "role-typed config.toml must not contain shell env overrides when mcp_shim is None; got:\n{s}"
        );
    }

    /// PR7a (#136) + #236 followup — `Some((shim, token))` injects a
    /// `[mcp_servers.calm]` block pointing at the resolved shim binary
    /// **and** an `[mcp_servers.calm.env]` table baking the socket
    /// path + per-card raw token into the shim subprocess env. The
    /// env-in-config-toml approach replaces the original "codex
    /// forwards the daemon env by default" assumption, which turned
    /// out to be wrong on codex CLI 0.132 (subprocesses got a clean
    /// env and the shim exited with `missing NEIGE_MCP_SOCKET`).
    #[test]
    fn role_config_toml_has_mcp_servers_block_when_shim_present() {
        let shim = crate::mcp_server::McpShimConfig {
            shim_bin: std::path::PathBuf::from("/usr/local/bin/neige-mcp-stdio-shim"),
            socket_path: std::path::PathBuf::from("/var/lib/neige/mcp/kernel.sock"),
        };
        let s = build_codex_config_toml_with_prompt(
            "/workspace",
            "you are a spec agent.",
            Some((&shim, "tok-abc123")),
        );
        assert!(
            s.contains("[mcp_servers.calm]"),
            "role-typed config.toml must contain the calm mcp_servers block when mcp_block is Some; got:\n{s}"
        );
        assert!(
            s.contains("command = \"/usr/local/bin/neige-mcp-stdio-shim\""),
            "shim binary path must appear as the command; got:\n{s}"
        );
        assert!(
            s.contains("[mcp_servers.calm.env]"),
            "role-typed config.toml must contain the calm mcp_servers env block (#236 followup); got:\n{s}"
        );
        assert!(
            s.contains("NEIGE_MCP_SOCKET = \"/var/lib/neige/mcp/kernel.sock\""),
            "env block must bake socket path so shim subprocess sees it; got:\n{s}"
        );
        assert!(
            s.contains("NEIGE_MCP_TOKEN = \"tok-abc123\""),
            "env block must bake per-card token so shim authenticates; got:\n{s}"
        );
        assert!(
            s.contains("[shell_environment_policy.set]"),
            "role-typed config.toml must force-inject MCP env into codex exec shells; got:\n{s}"
        );
        assert!(
            s.contains(
                "[shell_environment_policy.set]\nNEIGE_MCP_SOCKET = \"/var/lib/neige/mcp/kernel.sock\"\nNEIGE_MCP_TOKEN = \"tok-abc123\""
            ),
            "shell env override block must reuse the mcp socket/token values; got:\n{s}"
        );
    }

    /// #236 followup — the env block must escape `"` and `\` in both
    /// the socket path and the token. A pathological token containing
    /// `"` would otherwise close the TOML basic string mid-value and
    /// codex would reject the file with an opaque parse error.
    #[test]
    fn role_config_toml_escapes_env_block_values() {
        let shim = crate::mcp_server::McpShimConfig {
            shim_bin: std::path::PathBuf::from("/usr/local/bin/neige-mcp-stdio-shim"),
            // Forward slashes only on unix — a `"` in a path is
            // unusual but valid; defensive escape covers it.
            socket_path: std::path::PathBuf::from(r#"/tmp/odd"path/kernel.sock"#),
        };
        let s = build_codex_config_toml_with_prompt(
            "/workspace",
            "prompt",
            Some((&shim, r#"tok"with-quote"#)),
        );
        assert!(
            s.contains(r#"NEIGE_MCP_SOCKET = "/tmp/odd\"path/kernel.sock""#),
            "socket path with embedded quote must be escaped; got:\n{s}"
        );
        assert!(
            s.contains(r#"NEIGE_MCP_TOKEN = "tok\"with-quote""#),
            "token with embedded quote must be escaped; got:\n{s}"
        );
        assert_eq!(
            s.matches(r#"NEIGE_MCP_SOCKET = "/tmp/odd\"path/kernel.sock""#)
                .count(),
            2,
            "escaped socket should appear in mcp env and shell env blocks; got:\n{s}"
        );
        assert_eq!(
            s.matches(r#"NEIGE_MCP_TOKEN = "tok\"with-quote""#).count(),
            2,
            "escaped token should appear in mcp env and shell env blocks; got:\n{s}"
        );
    }

    #[test]
    fn role_config_toml_pre_trusts_cwd_and_bakes_prompt() {
        let s = build_codex_config_toml_with_prompt("/workspace", "you are a spec agent.", None);
        assert!(s.contains("approval_policy = \"never\""));
        assert!(s.contains("sandbox_mode = \"workspace-write\""));
        assert!(s.contains("[projects.\"/workspace\"]"));
        assert!(s.contains("trust_level = \"trusted\""));
        assert!(s.contains("instructions = \"you are a spec agent.\""));
    }

    #[test]
    fn role_config_toml_escapes_quotes_in_prompt() {
        // A prompt with a `"` would otherwise close the TOML basic string
        // mid-instructions.
        let s = build_codex_config_toml_with_prompt("/w", r#"say "hi""#, None);
        assert!(
            s.contains(r#"instructions = "say \"hi\"""#),
            "expected escaped quote inside instructions; got:\n{s}"
        );
    }

    #[test]
    fn role_config_toml_normalizes_newlines() {
        // The template contains real `\n`s; the rendered config.toml
        // uses `\n` escape sequences inside the basic string so the
        // file stays well-formed.
        let prompt = "line 1\nline 2";
        let s = build_codex_config_toml_with_prompt("/w", prompt, None);
        assert!(
            s.contains(r#"instructions = "line 1\nline 2""#),
            "newlines should be escaped in the basic string; got:\n{s}"
        );
    }
}
