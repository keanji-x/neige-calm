//! Mechanical structure shared by ordinary Tracks and scheduler-owned subtracks.
//! Callers own admission, Track row creation, initialization and transaction boundaries.

use crate::card_role_cache::CardRoleCache;
use crate::db::sqlite::{card_create_with_id_tx, overlay_upsert_tx};
use crate::error::{CalmError, Result};
use crate::event::{Event, EventScope};
use crate::ids::ActorId;
use crate::model::{Card, CardRole, NewCard, NewOverlay, Overlay, Track};
use crate::session_projection_repo::AgentProvider;
use crate::validation::CODEX_PAYLOAD_SCHEMA_VERSION;
use calm_types::track_report::TrackReportPayload;

/// A closed initialization contract: an independent Track supplies its prepared Planner
/// configuration; a subtrack supplies its seed and creation-time inherited configuration.
/// Admission and completion remain with the respective owner before/after this transaction step.
pub(crate) enum TrackInitialization {
    Independent {
        planner_payload: serde_json::Value,
    },
    Subtrack {
        seed: String,
        provider: AgentProvider,
        permission: crate::planner_permission_mode::PlannerPermissionMode,
        selection: crate::planner_model::CardModelSelection,
    },
}

pub(crate) struct PlannerReportStructure {
    pub(crate) report: Card,
    planner: Card,
    layout: Overlay,
}

impl PlannerReportStructure {
    /// Emit after caller-owned report initialization, so CardAdded carries its final payload.
    pub(crate) fn creation_events(
        self,
        track: &Track,
        actor: &ActorId,
        message: Option<String>,
    ) -> Vec<(ActorId, EventScope, Event)> {
        let track_scope = EventScope::Track {
            track: track.id.clone(),
            area: track.area_id.clone(),
        };
        let mut events = vec![(
            actor.clone(),
            track_scope.clone(),
            Event::TrackUpdated(crate::event::TrackUpdatedPayload::new(
                track.clone(),
                message,
            )),
        )];
        for card in [self.planner, self.report] {
            events.push((
                actor.clone(),
                EventScope::Card {
                    card: card.id.clone(),
                    track: track.id.clone(),
                    area: track.area_id.clone(),
                },
                Event::CardAdded(card),
            ));
        }
        events.push((actor.clone(), track_scope, Event::OverlaySet(self.layout)));
        events
    }
}

pub(crate) async fn initialize_track_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    track: &Track,
    ids: (&str, &str),
    initialization: TrackInitialization,
    role_cache: &CardRoleCache,
    host: &crate::acp_planner::config::AcpPlannerHost,
) -> Result<PlannerReportStructure> {
    let planner_payload = match initialization {
        TrackInitialization::Independent { planner_payload } => planner_payload,
        TrackInitialization::Subtrack {
            seed,
            provider,
            permission,
            selection,
        } => {
            let canonical = crate::harness::backend::factory::PlannerFactory::canonical_inputs(
                &crate::harness::profile::PlannerBinding {
                    profile: crate::harness::profile::HarnessProfile::Planner,
                    provider: provider.clone(),
                },
            );
            let mut payload = planner_payload(
                host,
                (!canonical).then_some(seed),
                provider,
                crate::planner_permission_mode::CreationSource::Inherited(permission),
            );
            selection.copy_to_payload(payload.as_object_mut().expect("planner payload"));
            payload
        }
    };
    let planner = card_create_with_id_tx(
        tx,
        ids.0.to_string(),
        NewCard {
            title: None,
            track_id: track.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: planner_payload,
        },
        CardRole::Planner,
        false,
        role_cache,
    )
    .await?;
    let report = card_create_with_id_tx(
        tx,
        ids.1.to_string(),
        NewCard {
            title: None,
            track_id: track.id.clone(),
            kind: "track-report".into(),
            sort: Some(-1.0),
            payload: serde_json::to_value(TrackReportPayload::initial())?,
        },
        CardRole::ReportCard,
        false,
        role_cache,
    )
    .await?;
    let layout = overlay_upsert_tx(
        tx,
        NewOverlay {
            plugin_id: "kernel".into(),
            entity_kind: "view".into(),
            entity_id: track.id.to_string(),
            kind: "layout".into(),
            payload: planner_harness_layout_payload(ids.0, ids.1),
        },
    )
    .await?;
    Ok(PlannerReportStructure {
        planner,
        report,
        layout,
    })
}

/// The payload production writes on a planner-harness card. `pub` so integration
/// fixtures can mint the production shape instead of a partial literal.
pub fn planner_harness_card_payload(
    host: &crate::acp_planner::config::AcpPlannerHost,
    goal: Option<String>,
    provider: AgentProvider,
) -> serde_json::Value {
    planner_payload(
        host,
        goal,
        provider,
        crate::planner_permission_mode::CreationSource::Independent,
    )
}

fn planner_payload(
    host: &crate::acp_planner::config::AcpPlannerHost,
    goal: Option<String>,
    provider: AgentProvider,
    source: crate::planner_permission_mode::CreationSource,
) -> serde_json::Value {
    let mut card_payload = serde_json::Map::new();
    card_payload.insert(
        "schemaVersion".into(),
        serde_json::Value::from(CODEX_PAYLOAD_SCHEMA_VERSION),
    );
    card_payload.insert(
        "codex_source".into(),
        serde_json::Value::String("shared".into()),
    );
    if let Some(goal) = goal.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        card_payload.insert("prompt".into(), serde_json::Value::String(goal.to_string()));
    }
    crate::planner_permission_mode::mint_at_creation(&mut card_payload, source, &provider, host);
    calm_truth::planner_binding::PlannerBinding {
        profile: calm_types::harness::HarnessProfile::Planner,
        provider,
    }
    .conversation_shape(serde_json::Value::Object(card_payload))
    .expect("Planner creation constructs an object payload")
    .1
}

pub(crate) fn planner_harness_layout_payload(
    planner_card_id: &str,
    report_card_id: &str,
) -> serde_json::Value {
    serde_json::json!({
        "schemaVersion": 1,
        "positions": {
            planner_card_id: {
                "x": 0, "y": 0, "w": 6, "h": 12
            },
            report_card_id: {
                "x": 6, "y": 0, "w": 6, "h": 12
            }
        }
    })
}

/// Allocate the empty carrier in the caller's creation transaction. This grants no
/// execution and reads the event floor before the caller appends its first wake.
pub(crate) async fn allocate_empty_planner_carrier_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    planner: &str,
    provider: AgentProvider,
) -> Result<String> {
    use crate::harness::profile::{HarnessProfile, PlannerBinding};
    use crate::harness::snapshot::CanonicalHarnessSnapshot;
    use crate::session_projection_repo::{WorkerSessionInit, WorkerSessionState};
    let shape = crate::db::sqlite::card_execution_shape_tx(tx, planner).await?;
    let actual = PlannerBinding::from_shape(&shape.kind, shape.role, &shape.payload)
        .ok_or_else(|| CalmError::Conflict("fresh Planner binding is invalid".into()))?;
    if actual.profile != HarnessProfile::Planner || actual.provider != provider {
        return Err(CalmError::Conflict(
            "fresh Planner differs from its requested binding".into(),
        ));
    }
    let already_owned: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM cards WHERE id=?1 AND session_id IS NOT NULL)",
    )
    .bind(planner)
    .fetch_one(&mut **tx)
    .await?;
    if already_owned {
        return Err(CalmError::Conflict(
            "fresh Planner already has an execution owner".into(),
        ));
    }
    let watermark: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(id),0) FROM events")
        .fetch_one(&mut **tx)
        .await?;
    let runtime = crate::model::new_id();
    crate::db::sqlite::session_start_runtime_tx(
        tx,
        WorkerSessionInit::shared_planner(
            runtime.clone(),
            planner.to_owned(),
            provider,
            WorkerSessionState::Idle,
            None,
            serde_json::to_value(CanonicalHarnessSnapshot::dormant(watermark))?,
            crate::model::now_ms(),
        ),
    )
    .await?;
    Ok(runtime)
}
