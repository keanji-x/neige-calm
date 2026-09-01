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
    WORKFLOW_TEMPLATES, is_workflow_template_key, task_payload_key_and_goal,
    workflow_template_report_from_payloads, workflow_template_task_payloads,
    workflow_template_task_payloads_from_body,
};
use axum::{
    Json, Router,
    extract::{Path, State},
    routing::get,
};
use calm_types::report_blocks::KIND_TASK;
use calm_types::report_blocks::tasks::key_is_valid;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/wave-templates", get(list_wave_templates))
        .route(
            "/api/wave-templates/{id}",
            get(get_wave_template_definition).put(update_wave_template),
        )
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

/// A template's editable definition: what Settings loads, edits and puts back.
///
/// Separate from [`WaveTemplate`] on purpose. The picker's shape is a *chooser*
/// view and #1209 argues down to `key` + `goal` for it deliberately; the editor
/// needs every field of the task or a save would silently drop the ones it
/// cannot see. Widening [`WaveTemplateTask`] to serve both would put acceptance
/// criteria and dependency edges into the New wave dialog's payload to satisfy
/// a surface that is not the New wave dialog.
#[derive(Debug, Serialize, ToSchema)]
pub struct WaveTemplateDefinition {
    pub id: String,
    /// Editable. Stored as the template wave's report summary — see the note on
    /// this module about why the wave row's own title is not involved.
    pub title: String,
    /// Every task, with every field. `context` / `gate` / `acceptance_criteria`
    /// are not editable in the UI, but they are returned and expected back so a
    /// save preserves them instead of flattening the task to `key` + `goal`.
    pub tasks: Vec<Value>,
    /// `true` once the template wave exists, i.e. once the title and tasks above
    /// came from the report the create path forks rather than from the built-in
    /// constants. Purely informational for the editor; a `PUT` works either way
    /// because it seeds first.
    pub seeded: bool,
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
    seeded: bool,
}

async fn current_definition(s: &RouteState, key: &str) -> Result<Definition> {
    // A seeded template's report is the authority. A *read failure* on it is an
    // error, never a reason to answer with the constants: the first cut used
    // `if let Ok(...)` here and so reported stale constant content with
    // `seeded: false` whenever the report card was unreadable — i.e. it turned
    // an outage into exactly the drift this endpoint exists to remove.
    if let Some(wave_id) = lookup_workflow_template_wave(s, key).await? {
        let (_, _, report) = resolve_report_for_wave(s.repo.as_ref(), &wave_id).await?;
        return Ok(Definition {
            title: report.summary.clone(),
            tasks: workflow_template_task_payloads_from_body(&report.body),
            seeded: true,
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
        seeded: false,
    })
}

fn definition_response(id: &str, definition: Definition) -> Result<WaveTemplateDefinition> {
    Ok(WaveTemplateDefinition {
        id: id.to_string(),
        title: definition.title,
        tasks: definition.tasks,
        seeded: definition.seeded,
    })
}

fn known_template(id: &str) -> Result<()> {
    is_workflow_template_key(id)
        .then_some(())
        .ok_or_else(|| CalmError::NotFound(format!("wave template `{id}`")))
}

#[utoipa::path(
    get,
    path = "/api/wave-templates/{id}",
    tag = "waves",
    params(("id" = String, Path, description = "Template key")),
    responses(
        (status = 200, description = "The template's editable definition", body = WaveTemplateDefinition),
        (status = 404, description = "Unknown template key", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn get_wave_template_definition(
    State(s): State<RouteState>,
    Path(id): Path<String>,
) -> Result<Json<WaveTemplateDefinition>> {
    known_template(&id)?;
    let definition = current_definition(&s, &id).await?;
    Ok(Json(definition_response(&id, definition)?))
}

/// A template edit from Settings.
#[derive(Debug, Deserialize, ToSchema)]
pub struct WaveTemplateUpdate {
    /// The new title. Trimmed; must not be empty — a template with a blank
    /// title is unpickable in the New wave dialog, which lists templates by
    /// title and nothing else.
    pub title: String,
    /// The new task list, in plan order. Each entry is a task object in the
    /// same shape `GET /api/wave-templates/{id}` returned, so the fields the
    /// editor does not display survive the round trip.
    ///
    /// An empty list is refused: a template *is* its task list, and forking an
    /// empty one would produce a wave whose plan is the intro paragraph alone.
    pub tasks: Vec<Value>,
}

#[utoipa::path(
    put,
    path = "/api/wave-templates/{id}",
    tag = "waves",
    params(("id" = String, Path, description = "Template key")),
    request_body = WaveTemplateUpdate,
    responses(
        (status = 200, description = "The stored definition after the edit", body = WaveTemplateDefinition),
        (status = 400, description = "Invalid title or task list", body = ErrorBody),
        (status = 404, description = "Unknown template key", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn update_wave_template(
    State(s): State<RouteState>,
    Path(id): Path<String>,
    Json(update): Json<WaveTemplateUpdate>,
) -> Result<Json<WaveTemplateDefinition>> {
    known_template(&id)?;
    let title = update.title.trim().to_string();
    if title.is_empty() {
        return Err(CalmError::BadRequest(
            "wave template: `title` must not be blank".to_string(),
        ));
    }
    if update.tasks.is_empty() {
        return Err(CalmError::BadRequest(
            "wave template: `tasks` must not be empty — a template is its task list".to_string(),
        ));
    }
    let tasks = validate_task_payloads(update.tasks)?;

    // Writing is the one path that may seed: a save has to have a wave to
    // write to. The read paths above still must not.
    ensure_workflow_templates(&s).await?;
    let wave_id = lookup_workflow_template_wave(&s, &id)
        .await?
        .ok_or_else(|| {
            CalmError::Internal(format!(
                "wave template: seeded template `{id}` is missing after ensure"
            ))
        })?;

    // Same renderer as the seeding path — never a second spelling of it here.
    let next = workflow_template_report_from_payloads(&id, &title, &tasks).ok_or_else(|| {
        CalmError::Internal(format!("wave template: no intro registered for `{id}`"))
    })?;
    let (wave, report_card, current) = resolve_report_for_wave(s.repo.as_ref(), &wave_id).await?;
    let if_doc_rev = current.doc_rev;
    // `WriteMarkdown`, not `Replace`. A template report is mostly `task`
    // fences, and the prose `Replace` path refuses by contract to modify or
    // delete a non-prose block — "the prose write/edit path may not touch data
    // blocks ... use calm.report.write_markdown for a whole-document rewrite".
    // Seeding gets away with `Replace` only because it writes into a report
    // that has no blocks yet; every save after the first is a rewrite of
    // existing task blocks and must declare itself as one.
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
    // what a subsequent GET will return, including anything `persist_report`'s
    // CRDT projection did to the text.
    let definition = current_definition(&s, &id).await?;
    Ok(Json(definition_response(&id, definition)?))
}

/// Validate the submitted task payloads.
///
/// Deliberately validates the **payload**, and does not deserialize it into
/// `PlanTaskInput`: that struct is `#[serde(deny_unknown_fields)]` and does not
/// model `refs` / `released_by_user` / `tombstone` / `spawn`, so routing the
/// write side through it would reject — or worse, silently drop — task blocks
/// the report contract accepts. The authority on a task block's shape is
/// `calm_types::report_blocks::validate_payload`, which is what the persist
/// boundary itself will apply; calling it here turns a whole-document rejection
/// into a per-task 400 that names the offending index.
///
/// The three checks on top are the ones the payload validator cannot express,
/// because they are about the *list*: key syntax, key uniqueness, and
/// dependencies that resolve inside this template.
fn validate_task_payloads(raw: Vec<Value>) -> Result<Vec<Value>> {
    use calm_types::report_blocks::validate_payload;

    let mut tasks = Vec::with_capacity(raw.len());
    for (index, mut value) in raw.into_iter().enumerate() {
        if !value.is_object() {
            return Err(CalmError::BadRequest(format!(
                "wave template: task {index} must be an object"
            )));
        }
        // The write side owns these two on a template, and the client is not
        // required to send them; stamp before validating so a payload that only
        // omits them is accepted rather than 400ing on a field the server sets.
        let tombstone = value.get("tombstone").is_some_and(|value| !value.is_null());
        if !tombstone {
            value["ready"] = Value::Bool(false);
            value["declared_by"] = Value::String("user".into());
        }
        validate_payload(KIND_TASK, &value).map_err(|error| {
            CalmError::BadRequest(format!("wave template: task {index} is invalid: {error}"))
        })?;
        tasks.push(value);
    }

    let mut seen = std::collections::BTreeSet::new();
    for (index, task) in tasks.iter().enumerate() {
        let Some(key) = task.get("key").and_then(Value::as_str) else {
            return Err(CalmError::BadRequest(format!(
                "wave template: task {index} has no key"
            )));
        };
        if !key_is_valid(key) {
            return Err(CalmError::BadRequest(format!(
                "wave template: task {index} has an invalid key `{key}`"
            )));
        }
        if !seen.insert(key) {
            return Err(CalmError::BadRequest(format!(
                "wave template: duplicate task key `{key}`"
            )));
        }
    }

    // A dependency on a key that is not in the list would render a task block
    // whose `depends_on` dangles — `unknown_deps` diagnoses that on every wave
    // forked from this template, i.e. the editor would be minting a broken plan
    // once per use instead of failing the one save that caused it.
    for task in &tasks {
        let key = task.get("key").and_then(Value::as_str).unwrap_or_default();
        for dependency in task
            .get("depends_on")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !seen.contains(dependency) {
                return Err(CalmError::BadRequest(format!(
                    "wave template: task `{key}` depends on `{dependency}`, which is not in the template"
                )));
            }
        }
    }
    Ok(tasks)
}
