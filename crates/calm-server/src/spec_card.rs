//! Spec-card binding (PR6 of #136).
//!
//! Every wave gets a single auto-minted **spec card** at create-time. The
//! spec card is the wave's "AI authority": the only card whose `AiSpec`
//! actor is allowed to emit `Event::WaveUpdated` (per `enforce_role`),
//! and the one whose Codex daemon runs with a system prompt scoped to
//! the wave's goal + acceptance criteria.
//!
//! This module owns the role-specific prompts and Codex environment
//! construction:
//!
//!   1. [`SPEC_SYSTEM_PROMPT_TEMPLATE`] — the system prompt used when
//!      starting the spec card's Codex thread. PR6 ships a minimal
//!      placeholder; PR7a flips on the kernel-as-MCP-server config
//!      block here.
//!
//! Atomicity story for the spec card itself lives in
//! `routes::waves::create_wave` — the spec card row and both
//! `Event::WaveUpdated` / `Event::CardAdded` envelopes are produced in a
//! single `write_with_events_typed` transaction.

use serde_json::Value;
use sqlx::{Sqlite, Transaction};

use crate::db::sqlite::card_update_tx;
use crate::error::{CalmError, Result};
use crate::ids::{CardId, WaveId};
use crate::model::CardPatch;
use crate::wave_vcs::CommitHash;

const LAST_SEEN_HEAD_KEY: &str = "last_seen_head";

pub(crate) fn last_seen_head_from_payload(payload: &Value) -> Option<CommitHash> {
    payload
        .get(LAST_SEEN_HEAD_KEY)
        .and_then(Value::as_str)
        .filter(|head| !head.is_empty())
        .map(ToOwned::to_owned)
}

pub(crate) async fn last_seen_head_for_card(
    repo: &dyn crate::db::RepoRead,
    card_id: &CardId,
) -> Result<Option<CommitHash>> {
    let Some(card) = repo.card_get(card_id.as_str()).await? else {
        return Ok(None);
    };
    Ok(last_seen_head_from_payload(&card.payload))
}

pub(crate) async fn stamp_last_seen_head_from_wave_head_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &CardId,
    wave_id: &WaveId,
) -> Result<()> {
    let current_head: Option<String> =
        sqlx::query_scalar("SELECT head_hash FROM wave_vcs_refs WHERE wave_id = ?1")
            .bind(wave_id.as_str())
            .fetch_optional(&mut **tx)
            .await?;
    stamp_last_seen_head_tx(tx, card_id, current_head.as_deref()).await
}

pub(crate) async fn stamp_last_seen_head_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &CardId,
    head: Option<&str>,
) -> Result<()> {
    let row: Option<(String,)> = sqlx::query_as("SELECT payload FROM cards WHERE id = ?1")
        .bind(card_id.as_str())
        .fetch_optional(&mut **tx)
        .await?;
    let Some((payload_text,)) = row else {
        return Ok(());
    };
    let mut payload: Value = serde_json::from_str(&payload_text).map_err(|e| {
        CalmError::Internal(format!(
            "spec card {card_id} payload is not valid JSON: {e}"
        ))
    })?;
    set_last_seen_head_in_payload(&mut payload, head)?;
    card_update_tx(
        tx,
        card_id.as_str(),
        CardPatch {
            kind: None,
            sort: None,
            payload: Some(payload),
            deletable: None,
        },
    )
    .await?;
    Ok(())
}

pub(crate) fn set_last_seen_head_in_payload(payload: &mut Value, head: Option<&str>) -> Result<()> {
    let Some(map) = payload.as_object_mut() else {
        return Err(CalmError::Internal(
            "spec card payload is not a JSON object".into(),
        ));
    };
    match head {
        Some(head) => {
            map.insert(
                LAST_SEEN_HEAD_KEY.to_string(),
                Value::String(head.to_string()),
            );
        }
        None => {
            map.remove(LAST_SEEN_HEAD_KEY);
        }
    }
    Ok(())
}

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

Lifecycle transitions are a side effect of every write. Pass \
`lifecycle=\"...\"` on `calm.task.dispatch`, \
`calm.task.verdict`, `calm.report.write`, or `calm.report.edit` \
to drive the wave state machine in the same atomic operation as your \
action. Every write also requires `message`, a short human-readable \
rationale for the event. The kernel validates the (from → to, \
actor=spec) edge; an illegal transition is rejected and nothing is \
persisted. The kernel auto-drives `draft → planning` on your first \
write, `dispatching → working` when a worker daemon spawns, and \
`working → reviewing` when the first task report lands.

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
   wave/card metadata; results are in `runs/*` views, not in `neige state`). \
   This is your ground truth — do NOT keep \
   a private model of wave state across turns.
2. Decide what to do next and act:
   * Dispatch sub-jobs via `calm.task.dispatch`. Required args: \
     `kind` (\"codex\" or \"terminal\"), `idempotency_key` (stable \
     across retries so a redelivered observation can't double-dispatch), \
     `message`, plus `goal` (codex) or `cmd` (terminal). Optional \
     `lifecycle` advances the wave in the same write.
   * Record verdicts via `calm.task.verdict(status=...)` when worker \
     output is ready to validate. Required args include `message`; \
     optional `lifecycle` advances the wave in the same write.
   * Keep the wave report current with `calm.report.write` or \
     `calm.report.edit`. Each requires `message` and accepts optional \
     `lifecycle`.
3. **END YOUR TURN.** Do NOT poll or loop waiting for the next event. \
   The kernel pushes the next observation as a fresh turn the moment it \
   arrives — you will be re-invoked automatically. If there is nothing \
   left to do this turn, just stop; if the wave is `done`/`failed`/ \
   `blocked` and you're waiting on the user, stop and wait to be \
   re-invoked.

## Wave Report (issue #229)

The wave has a user-facing Markdown report you maintain. The user sees \
it as the top card on the Wave page. Treat it like a file you keep \
updated. READ the current body with `neige cat report.md` (returns the \
report body). WRITE/EDIT via MCP tools that target the wave's report \
instead of a disk path:

  * `calm.report.write(body, summary?, message, lifecycle?)` — wholesale replace (like Write).
  * `calm.report.edit(old_string, new_string, replace_all?, message, lifecycle?)` — string \
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
    `task.verdict` accept/reject you recorded; `worker_card.payload` \
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
`calm.task.verdict(idempotency_key=K, status=\"accepted\" | \
\"rejected\")` to record a verdict, and/or `calm.task.dispatch` to \
start follow-up work. Each write requires `message` and can include \
`lifecycle=...`.

Wave is implicit — derived from your card identity. Do NOT pass a \
`wave_id` (these tools have no such parameter; cross-wave reads are \
forbidden by design).

Do not mint new spec cards from within this session.
";

/// Worker-agent system prompt. PR8 (#136) replaces the PR6 stub with
/// the production prompt: workers are short-lived, fire-and-forget,
/// driven by the spec card via `calm.task.dispatch`. They run one
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
3. When the task is done, report exactly once via the `neige` shell CLI:
   * On success: `neige task-completed --idempotency-key K --result <json-or-text>` \
     where `K` echoes the value from your spawning `*.worker_requested` event. \
     Append `--artifact <path>` (may repeat) for any file/blob references \
     you produced.
   * On failure: `neige task-failed --idempotency-key K --reason '<text>'` \
     with a free-form failure description.
4. Exit. You are short-lived by design — run your single job and stop. \
   The kernel delivers your `task.completed` / `task.failed` to the \
   spec card as a pushed turn input, and the spec continues the wave \
   from there. You do not wait for or observe anything.

You may NOT call `calm.task.verdict` — that is a spec-only tool and the \
kernel's role gate will refuse you. You also may NOT mint new workers \
via `calm.task.dispatch` — the \
kernel's role gate (#583) refuses worker-actor dispatch emits. If the \
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

/// Roles that legitimately need role-specific Codex setup.
/// Carved out of [`crate::model::CardRole`] so the seeding helper can
/// only ever be handed a value that maps to a system-prompt template
/// (no general Worker path to silently fall through). PR6 followup of
/// issue #136 — note 3 from the original review.
///
/// User-facing Worker cards still flow through `routes::codex_cards`'s
/// simpler seed path (which writes a no-prompt config.toml inline); they
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

    #[test]
    fn render_system_prompt_preserves_role_template_content() {
        let spec = render_system_prompt(SeededCardRole::Spec.prompt_template(), "wave-abc");
        assert!(spec.contains("You are the spec agent for wave `wave-abc`."));
        assert!(!spec.contains("calm.update_wave_state"));
        assert!(spec.contains("calm.task.dispatch"));
        assert!(spec.contains("calm.task.verdict"));

        let worker = render_system_prompt(SeededCardRole::Worker.prompt_template(), "wave-abc");
        assert!(worker.contains("You are a worker agent under spec card on wave `wave-abc`."));
        assert!(worker.contains("neige task-completed"));
    }

    /// #293 cutover — the spec prompt must be push-native, not pull. It must
    /// carry the turn-reactive guidance (driven by pushed observations, end
    /// the turn, no looping).
    #[test]
    fn spec_prompt_is_push_native_not_pull() {
        let p = SPEC_SYSTEM_PROMPT_TEMPLATE;

        // No pull loop.
        assert!(
            !p.contains("long-poll"),
            "prompt must not describe a long-poll loop"
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
            p.contains("Do NOT poll or loop"),
            "prompt must forbid polling / looping"
        );
        // Reads go through the shell CLI; writes still go through MCP.
        assert!(
            p.contains("Run `neige state`") && p.contains("calm.task.dispatch"),
            "prompt must read state via neige and still dispatch via MCP"
        );
        assert!(
            !p.contains("calm.update_wave_state")
                && p.contains("calm.task.dispatch")
                && p.contains("calm.task.verdict")
                && p.contains("calm.report.write")
                && p.contains("calm.report.edit"),
            "prompt must document retained wave/task write tools and omit retired update_wave_state"
        );
        assert!(
            !p.contains("Call `calm.wave.state`"),
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
            p.contains("neige task-completed") && p.contains("neige task-failed"),
            "worker prompt must document task completion through the neige CLI"
        );
        assert!(
            p.contains("READ-ONLY") && p.contains("own-wave-only"),
            "worker prompt must constrain neige reads to read-only own-wave views"
        );
    }
}
