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
//! wave's stored report. #1230 changed which of those two clauses survives: the
//! seeded report is the authority once it exists. #1300 S1 removed the *editor*
//! but deliberately left that read authority alone — see "Read-only" below for
//! why the two halves are separate slices.
//!
//! The *read must not trigger a write* half is unchanged and load-bearing:
//! `current_definition` looks the seeded wave up and gives up if it is absent;
//! it never calls `ensure_templates`. A `GET` that mints three waves the first
//! time somebody opens the New wave dialog is exactly what that forbids. A read
//! *failure* on a seeded report is propagated, never swallowed into the constant
//! fallback — answering with stale constants would turn an outage into drift.
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
//! ## Read-only, and why the read authority did not move with it (#1300 S1)
//!
//! `PUT /api/wave-templates/{id}` and the Settings › Templates editor existed
//! between #1230 and #1300. They were built on the seeded template wave: a
//! template report was an ordinary wave report, so a save was an ordinary
//! report write, and it inherited every invariant that write path has —
//! including `wave_report_edit_guard`'s #1179 task rules, which made the task
//! list append-only (a key is immutable for the life of its block, and a live
//! task may only leave as a tombstone).
//!
//! #1300 removes template seeding, because it is the last production path on
//! which the kernel writes a report as `EditAuthor::User`. The editor goes with
//! it: it has no storage of its own, only the hidden wave.
//!
//! **That removal is two slices, and this file is why the order is fixed.** S1
//! (this change) deletes the write side only. S2 deletes seeding and collapses
//! [`current_definition`] onto the constants. Doing both halves here would
//! leave a releasable state where this endpoint answers from the constants
//! while `POST /api/waves` still forks a report a `PUT` may have edited — the
//! picker showing one plan and create producing another, with both sides
//! internally consistent and no test red. So until S2 lands, the seeded report
//! stays the authority for what this endpoint reports.
//!
//! Making templates editable again is a real option, but it needs its own
//! persistence model and version semantics — not a wave borrowed as template
//! storage. Nothing here should grow a write side without that.
//!
//! **This module has no `PUT`, and `list_wave_templates` performs no write.**
//! `wave_templates_read.rs::put_is_not_routed_and_writes_nothing` pins both
//! halves; deleting a route without an assertion that it is gone is how a
//! removal quietly comes back.

use crate::error::{ErrorBody, Result};
use crate::routes::waves::{lookup_template_wave, resolve_template_binding};
use crate::state::{AppState, RouteState};
use crate::templates::{
    TEMPLATES, task_payload_key_and_goal, template_task_payloads, template_task_payloads_from_body,
};
use crate::wave_report::resolve_report_for_wave;
use axum::{Json, Router, extract::State, routing::get};
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/wave-templates", get(list_wave_templates))
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
