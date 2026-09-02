//! `GET /api/wave-templates` — the New wave picker's read side.
//!
//! ## Why this is an aggregate view and not a table (#1209)
//!
//! There is no `wave_templates` row anywhere. A template's facts live in two
//! authorities and this endpoint *joins* them; it never copies or invents a
//! third:
//!
//! * `id` / `title` — [`crate::templates::TEMPLATES`], the Rust constants
//!   `POST /api/waves` instantiates from.
//! * `input_schema` — the **owning plugin's** manifest `input_schema`, reached
//!   through the same [`resolve_template_binding`] the create path uses. Absent
//!   when no running trusted plugin declares that id, which is exactly the set
//!   of templates that would be rejected for carrying `template_input`.
//! * `tasks` — the template's own `task` blocks, projected to `key` + `goal`.
//!   The picker shows them so "what does this template give me" is answered
//!   with the template's own content instead of a prose description nobody
//!   owns (see below). The blocks are read as whole payloads and projected at
//!   the last moment (#1230); the projection also drops tombstones, which the
//!   picker must not advertise.
//!
//! `tasks` is read from the same recipe body `POST /api/waves` instantiates —
//! not from a second, typed list of keys and goals. That is the property worth
//! keeping: a task the picker advertises and a task the created wave contains
//! cannot differ, because there is nothing to keep in sync.
//!
//! #1230 briefly moved that authority into a stored report so an editor could
//! write to it; #1300 removed the editor (S1) and the stored report (S2), and
//! the read went back to the constants. **This endpoint performs no write of
//! any kind** — pinned by `wave_templates_read.rs`.
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
//! ## Read-only (#1300)
//!
//! `PUT /api/wave-templates/{id}` and the Settings › Templates editor existed
//! between #1230 and #1300. They were built on the seeded template wave: a
//! template report was an ordinary wave report, so a save was an ordinary
//! report write, and it inherited every invariant that write path has —
//! including `wave_report_edit_guard`'s #1179 task rules, which made the task
//! list append-only (a key is immutable for the life of its block, and a live
//! task may only leave as a tombstone).
//!
//! #1300 removed template seeding, because it was the last production path on
//! which the kernel wrote a report as `EditAuthor::User`. The editor went with
//! it: it had no storage of its own, only the hidden wave.
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
use crate::routes::waves::resolve_template_binding;
use crate::state::{AppState, RouteState};
use crate::templates::{TEMPLATES, task_payload_key_and_goal, template_task_payloads};
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
        let definition = current_definition(template.key);
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

/// A template's title and tasks: the Rust constants, and nothing else.
///
/// #1300 — this used to prefer the seeded template wave's stored report and
/// fall back to the constants only when the wave did not exist yet. That branch
/// existed for #1230's editor: once a template could be saved, the saved report
/// was what `POST /api/waves` forked, so reading the constants here would have
/// advertised a task list create did not produce.
///
/// Both sides of that are gone. S1 removed the editor; S2 removed the seeded
/// wave. `POST /api/waves` now instantiates the same constants this reads
/// (`routes::waves::prepare_template_report`), so there is one authority and
/// the drift the branch existed to prevent is not expressible.
///
/// Kept as a named function rather than inlined into the loop: it is the answer
/// to "what is this template, right now", and it having exactly one source is
/// the property worth being able to point at.
struct Definition {
    title: String,
    /// Whole task-block payloads, never a narrowed struct — see
    /// `template_task_payloads_from_body` for why that distinction is
    /// load-bearing rather than stylistic.
    tasks: Vec<Value>,
}

fn current_definition(key: &str) -> Definition {
    // `unwrap_or_default` is unreachable for a `TEMPLATES` key and stays a
    // default rather than a panic: both tables are keyed off the same
    // constants, and `listed_tasks_are_exactly_the_report_task_blocks` fails
    // loudly if one ever grows an entry the other lacks.
    Definition {
        title: TEMPLATES
            .iter()
            .find(|template| template.key == key)
            .map(|template| template.title.to_string())
            .unwrap_or_default(),
        tasks: template_task_payloads(key).unwrap_or_default(),
    }
}
