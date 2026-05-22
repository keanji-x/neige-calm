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
//!      host seed, write `hooks.json` + `config.toml`), but with a
//!      [`crate::model::CardRole`] discriminator so spec cards get the
//!      spec system prompt and worker cards get their own template
//!      (PR8 wires the worker prompt; PR6 leaves it as a stub).
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
use crate::model::CardRole;
use crate::routes::codex_cards::{build_hooks_json, copy_dir_recursive, host_codex_dir};
use crate::routes::terminal::spawn_daemon_for;
use crate::state::{AppState, CodexClient};

/// Minimal spec-agent system prompt template. PR6 ships a placeholder
/// that documents the role; PR7a/PR7b will expand this with explicit
/// instructions for the `wave_state.update` / `wave_state.get` MCP tools
/// once those land.
///
/// `{wave_id}` is the only substitution: at seed time the kernel replaces
/// it with the freshly minted wave id so the agent has a stable reference
/// for `wait_for_events` (PR8) calls.
///
/// Kept short on purpose: the codex CLI prepends this to every turn, so
/// every additional token is a per-turn cost. The substantive instructions
/// will arrive in the MCP tool descriptors that PR7b registers.
pub(crate) const SPEC_SYSTEM_PROMPT_TEMPLATE: &str = "\
You are the spec agent for wave `{wave_id}`.

Your responsibilities:
1. Read the wave's goal and acceptance criteria.
2. Decompose work into one or more sub-jobs via the `codex.job_requested` \
   or `terminal.job_requested` events. Each job request carries an \
   `idempotency_key` you must keep stable across retries.
3. Wait for `task.completed` / `task.failed` events that match your \
   idempotency keys (the kernel surfaces these via the `wait_for_events` \
   MCP tool, available from PR8).
4. Update the wave row (`Event::WaveUpdated`) only when the wave's state \
   genuinely changes — title, archive status, etc. Worker cards must NOT \
   touch the wave row; the kernel's role gate enforces this.

You are the wave's sole long-running AI authority. Do not mint new spec \
cards from within this session.
";

/// Worker-agent system prompt template. PR6 ships a stub so the role
/// plumbing compiles end-to-end; PR8 fills in the worker-specific
/// instructions (report `task.completed` with results, escalate on
/// failure, etc.).
pub(crate) const WORKER_SYSTEM_PROMPT_PLACEHOLDER: &str = "\
You are a worker agent operating under a spec card. (PR8 will replace this \
placeholder with the production worker prompt.)
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
    serde_json::Value::Object(env_map)
}

/// Per-spawn `$CODEX_HOME/config.toml` body, role-aware.
///
/// Same shape as `routes::codex_cards::build_codex_config_toml` but with
/// two additions:
///   * an `instructions = "<system_prompt>"` field — codex CLI reads
///     `~/.codex/config.toml` for `instructions` to prepend to every
///     turn; baking it here keeps spec/worker agents agent-typed
///     without an out-of-band registry.
///   * `[mcp_servers]` is still **omitted** in PR6. The
///     `config_toml_has_no_mcp_servers_block` regression test stays
///     green; PR7a will flip it for Spec/Worker.
///
/// Plain cards (the user-facing `POST /codex-cards` route) keep using
/// `routes::codex_cards::build_codex_config_toml` and pass no
/// `system_prompt`; this helper handles the role-typed paths.
pub(crate) fn build_codex_config_toml_with_prompt(cwd: &str, system_prompt: &str) -> String {
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
    format!(
        "# Generated by neige-calm per-spawn — silences codex's first-run\n\
         # dialogs so an auto-submitted \\r lands on the composer.\n\
         approval_policy = \"never\"\n\
         sandbox_mode = \"workspace-write\"\n\
         instructions = \"{one_line_prompt}\"\n\
         \n\
         [projects.\"{escaped_cwd}\"]\n\
         trust_level = \"trusted\"\n"
    )
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
///   3. (Re)write `hooks.json` so codex's lifecycle hooks point at
///      this server's bridge binary.
///   4. Write `config.toml` with the role-typed system prompt and
///      project trust.
///
/// Returns the resolved `CODEX_HOME` path so the caller can build the
/// env map (`CODEX_HOME = <path>`) for `spawn_daemon_for`.
///
/// `wave_id` is threaded in so the system prompt substitution can
/// reference the wave the card is bound to.
pub(crate) fn seed_codex_home_for_card(
    s: &AppState,
    card_id: &str,
    cwd: &str,
    wave_id: &str,
    role: CardRole,
) -> Result<PathBuf> {
    seed_codex_home_with_parts(s.codex.as_ref(), card_id, cwd, wave_id, role)
}

/// PR6 (#136) — lower-level seam over [`seed_codex_home_for_card`] that
/// takes a [`CodexClient`] directly. Used by the dispatcher, which
/// doesn't own an `AppState`.
pub(crate) fn seed_codex_home_with_parts(
    codex: &CodexClient,
    card_id: &str,
    cwd: &str,
    wave_id: &str,
    role: CardRole,
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

    // hooks.json — always (re)write. Cheap, and a stale path from a
    // previous spawn would otherwise break codex's hook dispatch.
    let bridge_path = codex.bridge_bin.to_string_lossy().to_string();
    let hooks_json = build_hooks_json(&bridge_path);
    let hooks_path = codex_home.join("hooks.json");
    std::fs::write(&hooks_path, hooks_json)
        .map_err(|e| CalmError::Internal(format!("write hooks.json: {e}")))?;

    // config.toml — role-typed. Spec and Worker cards bake the system
    // prompt directly into the file (codex reads `instructions` at
    // launch). Plain cards never reach this helper today; the
    // user-facing `routes::codex_cards` route writes its own config
    // (no system prompt) inline.
    let prompt_template = match role {
        CardRole::Spec => SPEC_SYSTEM_PROMPT_TEMPLATE,
        CardRole::Worker => WORKER_SYSTEM_PROMPT_PLACEHOLDER,
        CardRole::Plain => {
            // Shouldn't happen in PR6 — Plain cards go through
            // `routes::codex_cards`. Fall back to the unconditional
            // config toml the plain route uses.
            let cfg = crate::routes::codex_cards::build_codex_config_toml(cwd);
            let cfg_path = codex_home.join("config.toml");
            std::fs::write(&cfg_path, cfg)
                .map_err(|e| CalmError::Internal(format!("write config.toml: {e}")))?;
            return Ok(codex_home);
        }
    };
    let system_prompt = render_system_prompt(prompt_template, wave_id);
    let cfg_text = build_codex_config_toml_with_prompt(cwd, &system_prompt);
    let cfg_path = codex_home.join("config.toml");
    std::fs::write(&cfg_path, cfg_text)
        .map_err(|e| CalmError::Internal(format!("write config.toml: {e}")))?;

    Ok(codex_home)
}

/// PR6 (#136) — second-fix iteration: the response hot path on
/// `POST /api/waves` no longer awaits the spec card's CODEX_HOME seed
/// or `spawn_daemon_for` call. The handler commits the wave + spec
/// card + terminal transaction (atomic) and immediately returns 201;
/// this helper is then fired through [`tokio::spawn`] so the client
/// never blocks on the busy-poll-until-socket-ready window inside
/// `spawn_daemon_for` (~3s worst case when the daemon binary is
/// missing entirely, which is the test-env shape that broke the
/// `web/e2e/a11y-keyboard.spec.ts` 5s navigation timeout).
///
/// Failure handling: every error path is logged at `warn!` level.
/// Persisted rows (the spec card + its terminal) survive in the DB
/// regardless; the orphan-terminal sweeper reaps the dangling row
/// within ~60s. The caller (the background task) does not need to
/// react to errors beyond the log line.
///
/// Inputs are owned (`String` / `CardId` / `WaveId` / `serde_json::Value`)
/// so the spawned future is `'static`. The clones happen at the
/// call site (`AppState::clone()` is `Arc` bumps; the small string
/// allocations are O(card_id) + O(wave_id) + O(cwd), all sub-µs).
pub(crate) async fn seed_and_spawn_spec_daemon(
    state: AppState,
    spec_card_id: String,
    wave_id: String,
    cwd: String,
    env: serde_json::Value,
) {
    // 1. Seed `$CODEX_HOME` for the spec card. Filesystem-only — fast,
    //    bounded by a handful of mkdir + small write_alls. Failure
    //    here means hooks.json / config.toml didn't land; the daemon
    //    spawn below will still try, but the spec agent's instructions
    //    won't be loaded. Still better than 500ing the client.
    if let Err(e) = seed_codex_home_for_card(&state, &spec_card_id, &cwd, &wave_id, CardRole::Spec)
    {
        tracing::warn!(
            card_id = %spec_card_id,
            wave_id = %wave_id,
            error = %e,
            "spec card CODEX_HOME seed failed; orphan terminal will be reaped by sweeper",
        );
        return;
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
            return;
        }
        Err(e) => {
            tracing::warn!(
                card_id = %spec_card_id,
                wave_id = %wave_id,
                error = %e,
                "spec terminal lookup failed; orphan terminal will be reaped by sweeper",
            );
            return;
        }
    };

    // 3. Spawn the daemon. `codex` (no positional prompt) — the spec
    //    agent's system prompt is in $CODEX_HOME/config.toml's
    //    `instructions` field, not as a composer prefill.
    //
    //    `spawn_daemon_for` includes a busy-poll wait-until-socket-
    //    ready loop (up to ~3s); doing it off the response hot path
    //    is the whole point of this helper.
    if let Err(e) = spawn_daemon_for(&state, &term, "codex", &cwd, &env).await {
        tracing::warn!(
            card_id = %spec_card_id,
            wave_id = %wave_id,
            error = %e,
            "spec card daemon spawn failed; orphan terminal will be reaped by sweeper",
        );
        return;
    }

    tracing::info!(
        card_id = %spec_card_id,
        wave_id = %wave_id,
        terminal_id = %term.id,
        "spec card + daemon spawned for new wave (background task)"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Mirror of the plain `config_toml_has_no_mcp_servers_block`
    /// regression test on the role-typed config builder. PR7a flips
    /// this when `[mcp_servers.calm]` becomes the kernel-as-MCP-server
    /// glue.
    #[test]
    fn role_config_toml_has_no_mcp_servers_block() {
        let s = build_codex_config_toml_with_prompt("/workspace", "you are a spec agent.");
        assert!(
            !s.contains("[mcp_servers"),
            "role-typed config.toml must not contain mcp_servers blocks in PR6; got:\n{s}"
        );
    }

    #[test]
    fn role_config_toml_pre_trusts_cwd_and_bakes_prompt() {
        let s = build_codex_config_toml_with_prompt("/workspace", "you are a spec agent.");
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
        let s = build_codex_config_toml_with_prompt("/w", r#"say "hi""#);
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
        let s = build_codex_config_toml_with_prompt("/w", prompt);
        assert!(
            s.contains(r#"instructions = "line 1\nline 2""#),
            "newlines should be escaped in the basic string; got:\n{s}"
        );
    }
}
