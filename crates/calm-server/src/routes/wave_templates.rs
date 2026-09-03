//! `GET /api/wave-templates` — the New wave picker's read side.
//!
//! ## Why this is an aggregate view and not a table (#1209)
//!
//! There is no `wave_templates` row anywhere. A template's facts live in two
//! authorities and this endpoint *joins* them; it never copies or invents a
//! third:
//!
//! * `id` / `title` — [`crate::templates::TEMPLATES`], the
//!   Rust constants that also seed the template waves. Since #1230 the title is
//!   the constant only until the template is seeded; after that it is the
//!   seeded report's summary.
//! * `input_schema` — the **owning plugin's** manifest `input_schema`, reached
//!   through the same [`resolve_template_binding`] the create path uses. Absent
//!   when no running trusted plugin declares that id, which is exactly the set
//!   of templates that would be rejected for carrying `template_input`.
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
//! Deliberately **no `description`**: `templates.rs` has no such
//! field, and #1209 records that template facts are already spread across three
//! places. Adding a fourth spelling of "what this template is" to serve one
//! label is how the drift starts. The three titles are self-describing.
//!
//! ## The vocabulary seam, closed (#1209)
//!
//! One concept (template), one field (`template_id`). This endpoint lists it,
//! `POST /api/waves` admits by it, and there is no second spelling. The
//! `templates[]` array in a plugin manifest is a file written by *the other
//! side*; it declares which template keys this plugin claims. #1209 left that
//! array under its pre-unification name and called the difference a documented
//! adapter boundary; #1268 removed the difference instead — see the field docs
//! on `plugin_host::manifest::Manifest::templates`.
//!
//! The shape returned here did not change when the concepts merged.
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
//! The read stays a read. `current_definition` looks the wave up and gives up
//! if it is absent; it never calls `ensure_templates`. The lazy seed
//! is a write, and a `GET` that mints three waves the first time somebody opens
//! the New wave dialog is exactly the behaviour the note above forbids. A read
//! *failure* on a seeded report is propagated, never swallowed into the
//! constant fallback — answering with stale constants would turn an outage into
//! the very drift this file exists to remove.
//!
//! ### The title lives in the report summary, not the wave row
//!
//! A seeded template is a wave, so it has a `waves.title` column *and* a report
//! summary, both set to the constant title at seed time. The editor writes only
//! the summary, and this endpoint reads only the summary. Keeping the wave row
//! out of it means an edit is one write to one authority.
//!
//! ### The write endpoint is a diff, and that is the whole safety argument
//!
//! `PUT /api/wave-templates/{id}` takes `{title, edits:[{key,goal}],
//! appends:[{key,goal}]}` — never task blocks. Two earlier shapes accepted
//! blocks and both leaked, in ways that were fixed one at a time and kept
//! reappearing:
//!
//! * omitting a block deleted it. A live task was refused by the guard, but a
//!   **tombstone** was not (`guard_task_declarations`' removal check is gated on
//!   `!is_tombstone(old)`), so omitting one silently reversed a #1179-governed
//!   deletion and re-appending the key resurrected it;
//! * privileged vocabulary went in verbatim. Measured, not argued:
//!   `released_by_user: true` and `spawn: "sub-wave"` were both accepted and
//!   persisted.
//!
//! Both are one root cause — the editor was a second author of task blocks on a
//! document whose invariants assume one — and neither is fixable by adding
//! checks, because the check list must anticipate every field the task
//! vocabulary will ever grow. So the client states only `(key, goal)` and the
//! server owns everything else. The request structs are
//! `#[serde(deny_unknown_fields)]`: the guarantee is "there is nowhere to put
//! these", so an attempt is **refused**, not sanitised.
//!
//! ### The save is a document edit, not a regeneration
//!
//! The rebuild walks the report's **blocks** and re-emits each one with its
//! `<!-- neige:b_xxxx -->` marker. Both halves matter: rebuilding from the task
//! fences alone silently dropped every other block (`WriteMarkdown` is
//! documented as the op that *may* delete non-prose blocks, so nothing would
//! have refused it), and omitting the markers made `align.rs` re-derive block
//! identity from text similarity even though this handler knows exactly which
//! stored block each payload came from. Handing it the answer removes a class
//! of "the aligner guessed wrong" failures instead of betting on a threshold.
//!
//! ### The ceiling: append-only task lists
//!
//! A template report is an ordinary wave report, so `wave_report_edit_guard`'s
//! task-declaration invariants (#1179) apply to it in full: a task block's
//! `key` is immutable for the life of that block, and a live task may only
//! leave a document as a tombstone that `prepare_fork_report` then copies into
//! every wave forked afterwards.
//!
//! So a save may reword tasks and **append** them. It may not rename a key and
//! it may not remove a task. Editing a *retired* key is refused rather than
//! silently dropped — the projection hides tombstones, so a 200 there would
//! report success for a write that did not happen.
//!
//! Those invariants exist to protect a wave's live plan, and a template's tasks
//! are never live (`ready: false`, never projected). Relaxing them *for
//! templates only* is arguable, but it is a change to #1179's guard and not
//! something this endpoint may decide. Until then the limit is real, pinned by
//! `a_tombstone_survives_saves_and_its_key_stays_retired`, and the client must
//! not offer rename/delete affordances that can only 400.

use crate::error::{CalmError, ErrorBody, Result};
use crate::event::EditAuthor;
use crate::ids::ActorId;
use crate::routes::waves::{ensure_templates, lookup_template_wave, resolve_template_binding};
use crate::state::{AppState, RouteState};
use crate::templates::{
    AUTHOR_REAL_GATE, TEMPLATES, task_payload_key_and_goal, template_by_key,
    template_task_payloads, template_task_payloads_from_body,
};
use crate::wave_report::{ReportDocOp, persist_report_with_shadow, resolve_report_for_wave};
use axum::{
    Json, Router,
    extract::{Path, State},
    routing::{get, put},
};
use calm_types::report_blocks::tasks::key_is_valid;
use calm_types::report_blocks::{
    KIND_TASK, flat_text, marker_line, render_fence, validate_payload,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use utoipa::ToSchema;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/wave-templates", get(list_wave_templates))
        .route("/api/wave-templates/{id}", put(update_wave_template))
}

/// One selectable starting point for a new wave.
///
/// "Blank" is not in this list and never will be: it is the *absence* of a
/// template (`POST /api/waves` with no `template_id`), so the client renders it
/// as its own default option rather than the server minting a pseudo-row for
/// something that has no key, no title source, and no report to fork.
#[derive(Debug, Serialize, ToSchema)]
pub struct WaveTemplate {
    /// Template key. Passed back verbatim as `template_id` on
    /// `POST /api/waves` — see the seam note on this module.
    pub id: String,
    pub title: String,
    /// JSON Schema for `template_input`, from the manifest of the running
    /// trusted plugin bound to `id`. Absent means the template takes no input;
    /// sending `template_input` for it is a 400 on create.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<Value>,
    /// The tasks this template pre-sets, in plan order.
    ///
    /// Always present; **not** always non-empty. That was true while this came
    /// from the constants, but the projection drops tombstones, so retiring
    /// every task of a template (through the ordinary report block DELETE)
    /// leaves this empty. A client must render that state rather than assume it
    /// away.
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
    let mut templates = Vec::with_capacity(TEMPLATES.len());
    for template in &TEMPLATES {
        // Same resolver as create-time binding, so a template can never be
        // advertised with a schema the create path would then refuse to
        // validate against (stopped or untrusted plugin ⇒ `None` on both
        // sides).
        let input_schema = resolve_template_binding(&s, template.key)
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
/// Read-only. The `lookup_template_wave` miss is a plain "not seeded",
/// never a reason to seed — see this module's note.
struct Definition {
    title: String,
    /// Whole task-block payloads, never a narrowed struct — see
    /// `template_task_payloads_from_body` for why that distinction is
    /// load-bearing rather than stylistic.
    tasks: Vec<Value>,
}

async fn current_definition(s: &RouteState, key: &str) -> Result<Definition> {
    // A seeded template's report is the authority. A *read failure* on it is an
    // error, never a reason to answer with the constants: falling back would
    // report stale constant content as current, i.e. turn an outage into
    // exactly the drift this endpoint exists to remove.
    if let Some(wave_id) = lookup_template_wave(s, key).await? {
        let (_, _, report) = resolve_report_for_wave(s.repo.as_ref(), &wave_id).await?;
        return Ok(Definition {
            title: report.summary.clone(),
            tasks: template_task_payloads_from_body(&report.body),
        });
    }
    // `unwrap_or_default` is unreachable for a `TEMPLATES` key and
    // stays a default rather than a panic: both tables are keyed off the same
    // constants, and `listed_tasks_are_exactly_the_report_task_blocks` fails
    // loudly if one ever grows an entry the other lacks.
    Ok(Definition {
        title: TEMPLATES
            .iter()
            .find(|template| template.key == key)
            .map(|template| template.title.to_string())
            .unwrap_or_default(),
        tasks: template_task_payloads(key).unwrap_or_default(),
    })
}

/// #1209 PR-1 made `template_by_key()` the single fallible roster lookup and
/// deleted the second roster array it replaced. This goes through it rather
/// than re-deriving membership, so there is exactly one answer to "is this a
/// template" and the write endpoint cannot drift from the create path's admission.
fn known_template(id: &str) -> Result<()> {
    template_by_key(id)
        .map(|_| ())
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
#[serde(deny_unknown_fields)]
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
///
/// `deny_unknown_fields` is the load-bearing part, not decoration. The whole
/// safety argument for this endpoint is "privileged task vocabulary has nowhere
/// to go in the request"; without this attribute serde would quietly ignore
/// extra keys, the guarantee would rest on nobody ever adding a
/// `#[serde(flatten)]` here, and
/// `privileged_task_vocabulary_is_refused_by_the_request_shape` would keep passing
/// while the property it names had stopped holding.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
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
    ensure_templates(&s).await?;
    let wave_id = lookup_template_wave(&s, &id).await?.ok_or_else(|| {
        CalmError::Internal(format!(
            "wave template: seeded template `{id}` is missing after ensure"
        ))
    })?;
    let (wave, report_card, current) = resolve_report_for_wave(s.repo.as_ref(), &wave_id).await?;
    // The *blocks*, not the flat body. Two things follow from that and both are
    // load-bearing:
    //
    //  * every block is preserved, whatever its kind. Rebuilding from the task
    //    fences alone silently dropped prose the user had written and any
    //    non-task block the report happened to carry — `WriteMarkdown` is
    //    documented as the op that *may* delete non-prose blocks, so nothing
    //    would have refused it.
    //  * each block keeps its **id**, emitted as a `<!-- neige:b_xxxx -->`
    //    marker. Without markers `align.rs` re-derives identity from text
    //    similarity — a heuristic — even though this handler knows exactly
    //    which stored block each payload came from. Handing it the answer
    //    removes a whole class of "the aligner guessed wrong" failures
    //    (`key is immutable`, `must use the block-level DELETE endpoint`)
    //    instead of hoping the similarity stays above threshold.
    let blocks = current.blocks.clone().ok_or_else(|| {
        CalmError::Internal(format!(
            "wave template: seeded template `{id}` has no block projection"
        ))
    })?;

    // Every key a stored *live* task block declares, and separately the retired
    // ones. An append may reuse neither.
    // A task block with no string `key` lands in neither set. It is not
    // editable (nothing can name it) and not collidable (an append compares by
    // key), and the rebuild below re-emits it untouched with its marker — so it
    // survives rather than being quietly dropped.
    let mut live: BTreeMap<&str, usize> = BTreeMap::new();
    let mut retired: BTreeSet<&str> = BTreeSet::new();
    for block in &blocks {
        if block.kind != KIND_TASK {
            continue;
        }
        let Some(key) = block.payload.get("key").and_then(Value::as_str) else {
            continue;
        };
        if block
            .payload
            .get("tombstone")
            .is_some_and(|value| !value.is_null())
        {
            retired.insert(key);
        } else {
            *live.entry(key).or_insert(0) += 1;
        }
    }

    for edit in &update.edits {
        if retired.contains(edit.key.as_str()) {
            // A 200 here would report success for a write that did not happen:
            // the projection drops tombstones, so no client could tell.
            return Err(CalmError::BadRequest(format!(
                "wave template: task `{}` was retired and can no longer be edited",
                edit.key
            )));
        }
        match live.get(edit.key.as_str()) {
            None => {
                return Err(CalmError::BadRequest(format!(
                    "wave template: no task named `{}` in this template",
                    edit.key
                )));
            }
            // Duplicate keys are representable in a report (`dup_keys` is a
            // diagnostic, not a write-time refusal). Editing "that task" would
            // then rewrite both blocks with one goal — a coincidence, not a
            // decision. Refuse instead of guessing which one was meant.
            Some(&count) if count > 1 => {
                return Err(CalmError::BadRequest(format!(
                    "wave template: `{}` is declared by {count} live task blocks, so an edit \
                     cannot say which one it means; resolve the duplicate in the wave report first",
                    edit.key
                )));
            }
            Some(_) => {}
        }
    }
    let mut edited = BTreeSet::new();
    for edit in &update.edits {
        if !edited.insert(edit.key.as_str()) {
            return Err(CalmError::BadRequest(format!(
                "wave template: task `{}` is edited twice in one save",
                edit.key
            )));
        }
    }
    let goals: BTreeMap<&str, &str> = update
        .edits
        .iter()
        .map(|edit| (edit.key.as_str(), edit.goal.as_str()))
        .collect();

    let mut body = String::new();
    for block in &blocks {
        body.push_str(&marker_line(&block.id));
        if block.kind == KIND_TASK
            && block
                .payload
                .get("tombstone")
                .is_none_or(serde_json::Value::is_null)
            && let Some(key) = block.payload.get("key").and_then(Value::as_str)
            && let Some(goal) = goals.get(key)
        {
            let mut payload = block.payload.clone();
            payload["goal"] = Value::String((*goal).to_string());
            body.push_str(&render_fence(KIND_TASK, &payload));
        } else {
            body.push_str(&flat_text(block));
        }
        // A marker must start a line, so close an unterminated block — but do
        // not add a separator of our own. Each block's text already carries the
        // whitespace that separates it from the next, and appending one made a
        // save grow the body by a byte per block *every time*, including a save
        // that changed nothing: `a_no_op_save_leaves_the_body_byte_identical`.
        if !body.ends_with('\n') {
            body.push('\n');
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
        if live.contains_key(append.key.as_str())
            || retired.contains(append.key.as_str())
            || !appended.insert(append.key.clone())
        {
            return Err(CalmError::BadRequest(format!(
                "wave template: task key `{}` is already used in this template",
                append.key
            )));
        }
        // Constructed here, never accepted from the client. `no_gate_reason` is
        // the placeholder the kernel's own template constants carry, so an
        // appended task is not read as scheduled work missing a gate. No
        // marker: a new block must get a new id.
        let payload = json!({
            "key": append.key,
            "kind": "codex",
            "goal": append.goal,
            "depends_on": [],
            "no_gate_reason": AUTHOR_REAL_GATE,
            "ready": false,
            "declared_by": "user",
        });
        validate_payload(KIND_TASK, &payload).map_err(|error| {
            CalmError::BadRequest(format!(
                "wave template: appended task `{}` is invalid: {error}",
                append.key
            ))
        })?;
        body.push_str(&render_fence(KIND_TASK, &payload));
        body.push('\n');
    }

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
            summary: Some(title.clone()),
            body,
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
        input_schema: resolve_template_binding(&s, &id)
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
