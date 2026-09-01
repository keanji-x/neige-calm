//! `GET /api/wave-templates` — the New wave picker's read side.
//!
//! ## Why this is an aggregate view and not a table (#1209)
//!
//! There is no `wave_templates` row anywhere. A template's facts live in two
//! authorities and this endpoint *joins* them; it never copies or invents a
//! third:
//!
//! * `id` / `title` — [`crate::workflow_templates::WORKFLOW_TEMPLATES`], the
//!   Rust constants that also seed the template waves. Since #1230 the title is
//!   the constant only until the template is seeded; after that it is the
//!   seeded report's summary.
//! * `input_schema` — the **owning plugin's** manifest `input_schema`, reached
//!   through the same [`resolve_trusted_workflow`] the create path uses. Absent
//!   when no running trusted plugin declares that id, which is exactly the set
//!   of templates that would be rejected for carrying `workflow_input`.
//! * `tasks` — the template's own `task` blocks, projected to `key` + `goal`.
//!   The picker shows them so "what does this template give me" is answered
//!   with the template's own content instead of a prose description nobody
//!   owns (see below). Since #1230 the blocks are read as whole payloads and
//!   projected at the last moment; the projection also drops tombstones, which
//!   the picker must not advertise but the write side must hand back intact.
//!
//! `tasks` was read from a pure constant function and never from the template
//! wave's stored report. **#1230 changed which of those two clauses survives**:
//! the report is now the authority once it exists, but the *read must not
//! trigger a write* half is unchanged and load-bearing. See "Editable
//! templates" below.
//!
//! Deliberately **no `description`**: `workflow_templates.rs` has no such
//! field, and #1209 records that template facts are already spread across three
//! places. Adding a fourth spelling of "what this template is" to serve one
//! label is how the drift starts. The three titles are self-describing.
//!
//! ## The vocabulary seam, on purpose (#1209)
//!
//! This endpoint says **template**. The write side — `POST /api/waves` — still
//! says `workflow_id`, because on that field the name is accurate: it is the
//! thing the plugin-binding path resolves. #1209 rules that the two concepts
//! should merge into one (`template`, optionally bound to a plugin), and that
//! the merge waits for a forcing function. Until then the seam stays visible
//! and documented here rather than papered over with a `template_id` alias:
//! two wire fields doing one job is worse than one recorded seam.
//!
//! When the merge lands, the shape returned here does not change.
//!
//! ## Editable templates (#1230)
//!
//! The paragraph above — "`tasks` is read from a pure constant function" — was
//! true only while templates were read-only. They are now editable from
//! Settings, and that moves the authority:
//!
//! * **A template that has been seeded** — its `title` and `tasks` come from
//!   the system-cove template wave's report, because that report is what
//!   `POST /api/waves` actually forks (`waves.rs`, `fork_report_from`). Reading
//!   the constants here after an edit would make this endpoint advertise a task
//!   list the create path would not produce. That drift is the whole reason
//!   #1230 touches this file.
//! * **A template that has not been seeded yet** — the constants, unchanged.
//!
//! The read stays a read. `seeded_definition` looks the wave up and gives up if
//! it is absent; it never calls `ensure_workflow_templates`. The lazy seed is a
//! write, and a `GET` that mints three waves the first time somebody opens the
//! New wave dialog is exactly the behaviour the original note above forbids.
//!
//! ### The title lives in the report summary, not the wave row
//!
//! A seeded template is a wave, so it has a `waves.title` column *and* a report
//! summary, both set to the constant title at seed time. The editor writes only
//! the summary, and this endpoint reads only the summary. Keeping the wave row
//! out of it means an edit is one write to one authority: the same
//! `persist_report` call that carries the tasks. The system wave's own row title
//! is then a seeded-once display detail of a wave no UI ever lists — the system
//! cove is filtered out of `GET /api/coves` — and not a second place a title
//! could disagree from.
//!
//! ### What a save overwrites
//!
//! `PUT /api/wave-templates/{id}` re-renders the whole report as
//! `(constant intro + submitted tasks)`. A template report is *also* reachable
//! through the ordinary wave report editor, so prose somebody typed there is
//! **discarded by the next save from Settings**. That is a deliberate
//! trade — Settings is the editing authority for a template's shape, and the
//! alternative is a merge between two editors of the same document — but it is
//! a trade, not an accident, and it is why the editor round-trips the fields it
//! does not display (see `workflow_template_tasks_from_body`) instead of
//! rebuilding tasks from the two fields it does.
//!
//! ### The ceiling: append-only task lists
//!
//! A template report is an ordinary wave report, so `wave_report_edit_guard`'s
//! task-declaration invariants (#1179) apply to it in full:
//!
//! * a task block's `key` is **immutable** for the life of that block, and
//! * a live task may only leave a document through the block-level delete path,
//!   which `normalize_report_op` rewrites into an in-place tombstone for a
//!   `User` author — and `prepare_fork_report` then *copies* tombstones into
//!   every wave forked afterwards.
//!
//! So a save may change titles, goals, acceptance criteria, context and
//! dependency edges, and it may **append** tasks. It may not rename a key and
//! it may not remove a task: both come back as a 400 from the guard, which is
//! the correct outcome — the alternative is a template whose deletions
//! accumulate as tombstones in every future wave.
//!
//! Those invariants exist to protect a wave's live plan, and a template's tasks
//! are never live (`ready: false`, never projected). Relaxing them *for
//! templates only* is therefore arguable, but it is a change to #1179's guard
//! and not something this endpoint may decide on its own. Until then the limit
//! is real, pinned by
//! `renaming_or_removing_a_template_task_is_refused_by_the_report_contract`,
//! and the client must not offer rename/delete affordances that can only 400.

use crate::error::{CalmError, ErrorBody, Result};
use crate::event::EditAuthor;
use crate::ids::ActorId;
use crate::routes::waves::{
    ensure_workflow_templates, lookup_workflow_template_wave, resolve_trusted_workflow,
};
use crate::state::{AppState, RouteState};
use crate::wave_report::{ReportDocOp, persist_report_with_shadow, resolve_report_for_wave};
use crate::workflow_templates::{
    AUTHOR_REAL_GATE, WORKFLOW_TEMPLATES, is_workflow_template_key, task_payload_key_and_goal,
    workflow_template_report_from_payloads, workflow_template_task_payloads,
    workflow_template_task_payloads_from_body,
};
use axum::{
    Json, Router,
    extract::{Path, State},
    routing::{get, put},
};
use calm_types::report_blocks::tasks::key_is_valid;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use utoipa::ToSchema;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/wave-templates", get(list_wave_templates))
        .route("/api/wave-templates/{id}", put(update_wave_template))
}

/// One selectable starting point for a new wave.
///
/// "Blank" is not in this list and never will be: it is the *absence* of a
/// template (`POST /api/waves` with no `workflow_id`), so the client renders it
/// as its own default option rather than the server minting a pseudo-row for
/// something that has no key, no title source, and no report to fork.
#[derive(Debug, Serialize, ToSchema)]
pub struct WaveTemplate {
    /// Template key. Passed back verbatim as `workflow_id` on
    /// `POST /api/waves` — see the seam note on this module.
    pub id: String,
    pub title: String,
    /// JSON Schema for `workflow_input`, from the manifest of the running
    /// trusted plugin bound to `id`. Absent means the template takes no input;
    /// sending `workflow_input` for it is a 400 on create.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<Value>,
    /// The tasks this template pre-sets, in plan order. Always present and
    /// never empty for a real template — a template *is* its task list — so the
    /// client can show it without a "no tasks" branch that could never render.
    pub tasks: Vec<WaveTemplateTask>,
}

/// One pre-set task, projected from the template's own `PlanTaskInput`.
///
/// `key` and `goal` only: those are the two facts a person choosing a starting
/// point needs, and both are verbatim from the seeded plan. Acceptance
/// criteria, dependencies and gate advice belong to the wave's report once it
/// exists, not to the chooser.
#[derive(Debug, Serialize, ToSchema)]
pub struct WaveTemplateTask {
    /// The task block's `key` in the seeded report.
    pub key: String,
    /// What that task is for, verbatim from the template.
    pub goal: String,
}

#[utoipa::path(
    get,
    path = "/api/wave-templates",
    tag = "waves",
    responses(
        (status = 200, description = "Selectable wave templates", body = Vec<WaveTemplate>),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn list_wave_templates(
    State(s): State<RouteState>,
) -> Result<Json<Vec<WaveTemplate>>> {
    let mut templates = Vec::with_capacity(WORKFLOW_TEMPLATES.len());
    for template in &WORKFLOW_TEMPLATES {
        // Same resolver as create-time binding, so a template can never be
        // advertised with a schema the create path would then refuse to
        // validate against (stopped or untrusted plugin ⇒ `None` on both
        // sides).
        let input_schema = resolve_trusted_workflow(&s, template.key)
            .await
            .and_then(|manifest| manifest.input_schema.clone());
        let definition = current_definition(&s, template.key).await?;
        templates.push(WaveTemplate {
            id: template.key.to_string(),
            title: definition.title,
            input_schema,
            // Tombstoned blocks are dropped by the projection, not by the read:
            // the picker must not advertise a retired task, and the write side
            // must still hand the tombstone back untouched.
            tasks: definition
                .tasks
                .iter()
                .filter_map(task_payload_key_and_goal)
                .map(|(key, goal)| WaveTemplateTask { key, goal })
                .collect(),
        });
    }
    Ok(Json(templates))
}

/// The current authority for a template's title and tasks: the seeded report if
/// there is one, the built-in constants otherwise.
///
/// Read-only. The `lookup_workflow_template_wave` miss is a plain "not seeded",
/// never a reason to seed — see this module's note.
struct Definition {
    title: String,
    /// Whole task-block payloads, never a narrowed struct — see
    /// `workflow_template_task_payloads_from_body` for why that distinction is
    /// load-bearing rather than stylistic.
    tasks: Vec<Value>,
}

async fn current_definition(s: &RouteState, key: &str) -> Result<Definition> {
    // A seeded template's report is the authority. A *read failure* on it is an
    // error, never a reason to answer with the constants: falling back would
    // report stale constant content as current, i.e. turn an outage into
    // exactly the drift this endpoint exists to remove.
    if let Some(wave_id) = lookup_workflow_template_wave(s, key).await? {
        let (_, _, report) = resolve_report_for_wave(s.repo.as_ref(), &wave_id).await?;
        return Ok(Definition {
            title: report.summary.clone(),
            tasks: workflow_template_task_payloads_from_body(&report.body),
        });
    }
    // `unwrap_or_default` is unreachable for a `WORKFLOW_TEMPLATES` key and
    // stays a default rather than a panic: both tables are keyed off the same
    // constants, and `listed_tasks_are_exactly_the_report_task_blocks` fails
    // loudly if one ever grows an entry the other lacks.
    Ok(Definition {
        title: WORKFLOW_TEMPLATES
            .iter()
            .find(|template| template.key == key)
            .map(|template| template.title.to_string())
            .unwrap_or_default(),
        tasks: workflow_template_task_payloads(key).unwrap_or_default(),
    })
}

fn known_template(id: &str) -> Result<()> {
    is_workflow_template_key(id)
        .then_some(())
        .ok_or_else(|| CalmError::NotFound(format!("wave template `{id}`")))
}

/// A template edit from Settings — a **diff**, never a task list.
///
/// ## Why the client cannot send task payloads (#1230 review round 2)
///
/// The first two cuts took the whole task list back. Both leaked, in ways that
/// were fixed one at a time and kept reappearing in a new shape:
///
/// * a client that simply **omitted** a task erased it. For a live task the
///   guard refused the write, but for a **tombstone** it did not —
///   `guard_task_declarations`' removal check is gated on `!is_tombstone(old)`
///   — so omitting a tombstone silently reversed a #1179-governed deletion, and
///   re-appending the key resurrected it.
/// * a client could put privileged vocabulary into a payload the server then
///   stored verbatim. Measured, not argued: `released_by_user: true` and
///   `spawn: "sub-wave"` were both accepted and persisted.
///
/// Both are the same root cause — the editor was a second author of task
/// blocks on a document whose invariants assume one — and neither is fixable by
/// adding checks, because the check list has to anticipate every field the task
/// vocabulary will ever grow.
///
/// So the write side no longer accepts blocks at all. It accepts *what changed*:
/// a title, goals for keys that already exist, and tasks to append. The server
/// reads the stored payloads, edits them in place and constructs appended ones
/// itself. Omission is not expressible, privileged fields are not expressible,
/// and a rename is not expressible — all three are structurally impossible
/// rather than rejected.
#[derive(Debug, Deserialize, ToSchema)]
pub struct WaveTemplateUpdate {
    /// The new title. Trimmed; must not be empty — a template with a blank
    /// title is unpickable in the New wave dialog, which lists templates by
    /// title and nothing else.
    pub title: String,
    /// New goals for tasks that already exist, keyed by the task's `key`.
    /// A key the template does not declare is a 400, not a silent create.
    #[serde(default)]
    pub edits: Vec<WaveTemplateGoalEdit>,
    /// Tasks to add, in the order they should appear after the existing ones.
    #[serde(default)]
    pub appends: Vec<WaveTemplateGoalEdit>,
}

/// One `(key, goal)` pair — the only two facts the editor may state about a
/// task. Everything else about a task block is the server's.
#[derive(Debug, Deserialize, ToSchema)]
pub struct WaveTemplateGoalEdit {
    pub key: String,
    pub goal: String,
}

#[utoipa::path(
    put,
    path = "/api/wave-templates/{id}",
    tag = "waves",
    params(("id" = String, Path, description = "Template key")),
    request_body = WaveTemplateUpdate,
    responses(
        (status = 200, description = "The template as stored after the edit", body = WaveTemplate),
        (status = 400, description = "Invalid title, unknown key, or duplicate append", body = ErrorBody),
        (status = 404, description = "Unknown template key", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn update_wave_template(
    State(s): State<RouteState>,
    Path(id): Path<String>,
    Json(update): Json<WaveTemplateUpdate>,
) -> Result<Json<WaveTemplate>> {
    known_template(&id)?;
    let title = update.title.trim().to_string();
    if title.is_empty() {
        return Err(CalmError::BadRequest(
            "wave template: `title` must not be blank".to_string(),
        ));
    }
    for edit in update.edits.iter().chain(&update.appends) {
        if edit.goal.trim().is_empty() {
            return Err(CalmError::BadRequest(format!(
                "wave template: task `{}` has a blank goal",
                edit.key
            )));
        }
    }

    // Writing is the one path that may seed: a save has to have a wave to write
    // to. The read paths must not.
    ensure_workflow_templates(&s).await?;
    let wave_id = lookup_workflow_template_wave(&s, &id)
        .await?
        .ok_or_else(|| {
            CalmError::Internal(format!(
                "wave template: seeded template `{id}` is missing after ensure"
            ))
        })?;
    let (wave, report_card, current) = resolve_report_for_wave(s.repo.as_ref(), &wave_id).await?;
    let mut payloads = workflow_template_task_payloads_from_body(&current.body);

    // Every existing key, including tombstoned ones: an append may not collide
    // with a retired key either, or the block-level history for that key would
    // have two authors.
    let existing: BTreeSet<String> = payloads
        .iter()
        .filter_map(|payload| payload.get("key").and_then(Value::as_str))
        .map(str::to_string)
        .collect();

    for edit in &update.edits {
        if !existing.contains(&edit.key) {
            return Err(CalmError::BadRequest(format!(
                "wave template: no task named `{}` in this template",
                edit.key
            )));
        }
        for payload in &mut payloads {
            // Tombstones are skipped rather than 400'd: they are not editable,
            // they are not shown, and they must come through untouched.
            if payload
                .get("tombstone")
                .is_some_and(|value| !value.is_null())
            {
                continue;
            }
            if payload.get("key").and_then(Value::as_str) == Some(edit.key.as_str()) {
                payload["goal"] = Value::String(edit.goal.clone());
            }
        }
    }

    let mut appended = BTreeSet::new();
    for append in &update.appends {
        if !key_is_valid(&append.key) {
            return Err(CalmError::BadRequest(format!(
                "wave template: invalid task key `{}`",
                append.key
            )));
        }
        if existing.contains(&append.key) || !appended.insert(append.key.clone()) {
            return Err(CalmError::BadRequest(format!(
                "wave template: task key `{}` is already used in this template",
                append.key
            )));
        }
        // Constructed here, not accepted from the client. `no_gate_reason` is
        // the same placeholder the kernel's own template constants carry, so an
        // appended task is not read as scheduled work missing a gate.
        payloads.push(json!({
            "key": append.key,
            "kind": "codex",
            "goal": append.goal,
            "depends_on": [],
            "no_gate_reason": AUTHOR_REAL_GATE,
            "ready": false,
            "declared_by": "user",
        }));
    }

    let next = workflow_template_report_from_payloads(&id, &title, &payloads).ok_or_else(|| {
        CalmError::Internal(format!("wave template: no intro registered for `{id}`"))
    })?;
    let if_doc_rev = current.doc_rev;
    // `WriteMarkdown`, not `Replace`. A template report is mostly `task`
    // fences, and the prose `Replace` path refuses by contract to modify or
    // delete a non-prose block. Seeding gets away with `Replace` only because
    // it writes into a report that has no blocks yet; every save after the
    // first rewrites existing task blocks and must declare itself as one.
    persist_report_with_shadow(
        s.repo.as_ref(),
        &s.events,
        &s.write,
        ActorId::User,
        EditAuthor::User,
        wave,
        report_card,
        current,
        ReportDocOp::WriteMarkdown {
            summary: Some(next.summary),
            body: next.body,
            if_doc_rev,
        },
        None,
        None,
        false,
        None,
    )
    .await?;

    // Re-read rather than echo what we just built: the response then states
    // what a subsequent read will return.
    let definition = current_definition(&s, &id).await?;
    Ok(Json(WaveTemplate {
        id: id.clone(),
        title: definition.title,
        input_schema: resolve_trusted_workflow(&s, &id)
            .await
            .and_then(|manifest| manifest.input_schema.clone()),
        tasks: definition
            .tasks
            .iter()
            .filter_map(task_payload_key_and_goal)
            .map(|(key, goal)| WaveTemplateTask { key, goal })
            .collect(),
    }))
}
