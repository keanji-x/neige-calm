//! `GET /api/track-templates` — the New track picker's read side.
//!
//! ## Why this is an aggregate view and not a table (#1209)
//!
//! There is no `track_templates` row anywhere. A template's facts live in two
//! authorities and this endpoint *joins* them; it never copies or invents a
//! third:
//!
//! * `id` / `title` — [`crate::templates::TEMPLATES`], the Rust constants
//!   `POST /api/tracks` instantiates from.
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
//! `tasks` is read from the same recipe `POST /api/tracks` instantiates — not
//! from a second, typed list of keys and goals. That is the property worth
//! keeping: a task the picker advertises and a task the created track contains
//! cannot differ, because there is nothing to keep in sync.
//!
//! #1321 S3 made that share the *compiler* and not just the bytes: both roads
//! go through `routes::tracks::compile_template`, and this endpoint projects
//! the picker's task list off the blocks that call produced. Before it, this
//! endpoint re-parsed the rendered body with the lenient `split_body` reader
//! and swallowed the difference with `unwrap_or_default()`. Reading that
//! deleted code, a recipe create refuses with 500 could be advertised here as
//! a 200 with a shortened task list; that is a mechanism read off the code, not
//! a measurement — see the note on `current_definition` for what was actually
//! run. The paragraph is about this endpoint, `GET /api/track-templates`, and
//! the built-in roster only — user recipes (`GET /api/track-recipes`, #1292)
//! are a different read with a different, `BadRequest`-shaped write boundary.
//!
//! #1230 briefly moved that authority into a stored report so an editor could
//! write to it; #1300 removed the editor (S1) and the stored report (S2), and
//! the read went back to the constants. **This endpoint performs no write of
//! any kind** — pinned by `track_templates_read.rs`.
//!
//! Deliberately **no `description`**: `templates.rs` has no such
//! field, and #1209 records that template facts are already spread across three
//! places. Adding a fourth spelling of "what this template is" to serve one
//! label is how the drift starts. The three titles are self-describing.
//!
//! ## The vocabulary seam, closed (#1209)
//!
//! One concept (template), one field (`template_id`). This endpoint lists it,
//! `POST /api/tracks` admits by it, and there is no second spelling. The
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
//! `PUT /api/track-templates/{id}` and the Settings › Templates editor existed
//! between #1230 and #1300. They were built on the seeded template track: a
//! template report was an ordinary track report, so a save was an ordinary
//! report write, and it inherited every invariant that write path has —
//! including `track_report_edit_guard`'s #1179 task rules, which made the task
//! list append-only (a key is immutable for the life of its block, and a live
//! task may only leave as a tombstone).
//!
//! #1300 removed template seeding, because it was the last production path on
//! which the kernel wrote a report as `EditAuthor::User`. The editor went with
//! it: it had no storage of its own, only the hidden track.
//!
//! Making templates editable again is a real option, but it needs its own
//! persistence model and version semantics — not a track borrowed as template
//! storage. Nothing here should grow a write side without that.
//!
//! **This module has no `PUT`, and `list_track_templates` performs no write.**
//! `track_templates_read.rs::put_is_not_routed_and_writes_nothing` pins both
//! halves; deleting a route without an assertion that it is gone is how a
//! removal quietly comes back.

use crate::error::{ErrorBody, Result};
use crate::routes::tracks::{compile_template, resolve_template_binding};
use crate::state::{AppState, RouteState};
use crate::templates::{TEMPLATES, Template, task_payload_key_and_instruction};
use axum::{Json, Router, extract::State, routing::get};
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/track-templates", get(list_track_templates))
}

/// One selectable starting point for a new track.
///
/// "Blank" is not in this list and never will be: it is the *absence* of a
/// template (`POST /api/tracks` with no `template_id`), so the client renders it
/// as its own default option rather than the server minting a pseudo-row for
/// something that has no key, no title source, and no report to fork.
#[derive(Debug, Serialize, ToSchema)]
pub struct TrackTemplate {
    /// Template key. Passed back verbatim as `template_id` on
    /// `POST /api/tracks` — see the seam note on this module.
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
    pub tasks: Vec<TrackTemplateTask>,
}

/// One pre-set task, projected from the template's own `PlanTaskInput`.
///
/// `key` and `goal` only: those are the two facts a person choosing a starting
/// point needs, and both are verbatim from the recipe's own `task` block.
/// Acceptance criteria, dependencies and gate advice belong to the track's
/// report once it exists, not to the chooser.
#[derive(Debug, Serialize, ToSchema)]
pub struct TrackTemplateTask {
    /// The task block's `key` in the recipe this template instantiates to.
    pub key: String,
    /// What that task is for, verbatim from the template.
    pub goal: String,
}

#[utoipa::path(
    get,
    path = "/api/track-templates",
    tag = "tracks",
    responses(
        (status = 200, description = "Selectable track templates", body = Vec<TrackTemplate>),
        // #1321 S3 — reachable, not boilerplate: a roster recipe that does not
        // compile answers 500 here for the same reason `POST /api/tracks`
        // answers 500 for it, instead of a 200 with an empty title and a
        // shortened task list.
        (status = 500, description = "A built-in recipe did not compile", body = ErrorBody),
    ),
)]
pub(crate) async fn list_track_templates(
    State(s): State<RouteState>,
) -> Result<Json<Vec<TrackTemplate>>> {
    let mut templates = Vec::with_capacity(TEMPLATES.len());
    for template in &TEMPLATES {
        // Same resolver as create-time binding, so a template can never be
        // advertised with a schema the create path would then refuse to
        // validate against (stopped or untrusted plugin ⇒ `None` on both
        // sides).
        let input_schema = resolve_template_binding(&s, template)
            .await
            .and_then(|manifest| manifest.input_schema.clone());
        let definition = current_definition(template)?;
        templates.push(TrackTemplate {
            id: template.key().to_string(),
            title: definition.title,
            input_schema,
            // Tombstoned blocks are dropped by the projection, not by the read:
            // the picker must not advertise a retired task, and the write side
            // must still hand the tombstone back untouched.
            tasks: definition
                .tasks
                .iter()
                .filter_map(task_payload_key_and_instruction)
                .map(|(key, goal)| TrackTemplateTask { key, goal })
                .collect(),
        });
    }
    Ok(Json(templates))
}

/// A template's title and tasks: the Rust constants, and nothing else.
///
/// #1300 — this used to prefer the seeded template track's stored report and
/// fall back to the constants only when the track did not exist yet. That branch
/// existed for #1230's editor: once a template could be saved, the saved report
/// was what `POST /api/tracks` forked, so reading the constants here would have
/// advertised a task list create did not produce.
///
/// Both sides of that are gone. S1 removed the editor; S2 removed the seeded
/// track. `POST /api/tracks` now instantiates the same constants this reads
/// (`routes::tracks::prepare_template_report`), so there is one authority and
/// the drift the branch existed to prevent is not expressible.
///
/// Kept as a named function rather than inlined into the loop: it is the answer
/// to "what is this template, right now", and it having exactly one source is
/// the property worth being able to point at.
struct Definition {
    title: String,
    /// Whole task-block payloads, never a narrowed struct — the projection to
    /// `key` + `goal` happens at the last moment, in the handler.
    tasks: Vec<Value>,
}

/// #1321 S3 — fallible, and reading the *compiled* recipe.
///
/// Both changes are one change. This used to call `template_task_payloads`,
/// which re-parsed the rendered recipe body leniently, and then
/// `unwrap_or_default()` on both halves. Neither degradation was reachable
/// through a bad request — the only input is a roster entry — so both could
/// only ever fire on a kernel defect, and both answered it with a 200 carrying
/// an empty title or a silently shortened task list. `POST /api/tracks` refuses
/// the same recipe with `CalmError::Internal`
/// (`routes::tracks::prepare_template_report`); this endpoint now fails the same
/// way, off the same `compile_template` call, so the picker cannot advertise a
/// template create would refuse.
///
/// KNOWN GAP — no automated test covers the error arm. The handler iterates
/// the [`TEMPLATES`] static itself, so covering it would mean making one of the
/// three roster constants fail to compile at test time: there is no seam to
/// inject an entry through, and in **safe** Rust no substitute can be built
/// either, since every field of `Template` — `build_recipe` included — is
/// private and a literal outside `templates.rs` is `E0451`. That safe-Rust
/// scope is the one `templates::Template`'s own doc states and no wider; a
/// `transmute`-built entry is outside it, with the consequences registered on
/// `routes::tracks::admit_template`. It was verified by mutation instead:
/// inserting an indented neige-block `task` fence opener into
/// `SMALL_CHANGE_INTRO` turned
/// `track_templates_read::lists_every_template_with_its_kernel_title` red — a
/// case that issues only `GET /api/track-templates` and no create — with
/// `left: 500, right: 200` and the response body "internal: track create:
/// template `small-change` body: bad request: indented neige-block opener at
/// byte 2785". Three sibling picker cases went red the same way.
///
/// What that measures and what it does not: it measures that this endpoint now
/// answers 500 for a recipe the create path also refuses. It does **not**
/// measure the pre-#1321 answer to the same mutation — the reason to expect a
/// 200 there is that the old read went through `split_body`, which demotes an
/// indented opener to prose (`templates::
/// body_prose_and_foreign_fences_are_skipped_not_parsed`), and that is
/// inference, not a run.
fn current_definition(template: &Template) -> Result<Definition> {
    let compiled = compile_template(template)?;
    Ok(Definition {
        title: template.title().to_string(),
        tasks: compiled
            .task_block_payloads()?
            .into_iter()
            .cloned()
            .collect(),
    })
}
