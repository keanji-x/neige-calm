//! `GET /api/wave-templates` — the New wave picker's read side.
//!
//! ## Why this is an aggregate view and not a table (#1209)
//!
//! There is no `wave_templates` row anywhere. A template's facts live in two
//! authorities and this endpoint *joins* them; it never copies or invents a
//! third:
//!
//! * `id` / `title` — [`crate::workflow_templates::WORKFLOW_TEMPLATES`], the
//!   Rust constants that also seed the template waves.
//! * `input_schema` — the **owning plugin's** manifest `input_schema`, reached
//!   through the same [`resolve_trusted_workflow`] the create path uses. Absent
//!   when no running trusted plugin declares that id, which is exactly the set
//!   of templates that would be rejected for carrying `workflow_input`.
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

use crate::error::{ErrorBody, Result};
use crate::routes::waves::resolve_trusted_workflow;
use crate::state::{AppState, RouteState};
use crate::workflow_templates::WORKFLOW_TEMPLATES;
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
        templates.push(WaveTemplate {
            id: template.key.to_string(),
            title: template.title.to_string(),
            input_schema,
        });
    }
    Ok(Json(templates))
}
