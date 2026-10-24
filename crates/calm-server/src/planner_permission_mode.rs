//! A Planner card's permission mode (#2348): its creation sources and stored value.
//! The harness reads it when it issues a turn
//! (`harness::run_loop::turn_permission`) and the provider adapter turns it into its own settings.
//! After creation, the User-only PUT route edits the stored value; Planner switching
//! inherits the outgoing Planner's stored mode in the switch transaction.

use serde_json::{Map, Value};

pub use calm_types::harness::PlannerPermissionMode;

use crate::acp_planner::config::AcpPlannerHost;
use crate::harness::profile::{HarnessProfile, PlannerBinding};
use crate::model::{Card, CardRole};
use crate::session_projection_repo::AgentProvider;
use crate::validation::PLANNER_PERMISSION_MODE_PAYLOAD_KEY;

/// Independent tracks use their provider registration default. Delegation explicitly copies the
/// parent's creation-time mode, including ask/full; later parent edits do not propagate.
pub(crate) enum CreationSource {
    Independent,
    Inherited(PlannerPermissionMode),
}

/// Stamp the admitted creation source exactly once, without a temporary default.
pub(crate) fn mint_at_creation(
    payload: &mut Map<String, Value>,
    source: CreationSource,
    provider: &AgentProvider,
    host: &AcpPlannerHost,
) {
    let mode = match source {
        CreationSource::Independent => host
            .configured(provider)
            .ok()
            .and_then(|registration| registration.default_permission_mode)
            .unwrap_or(PlannerPermissionMode::Never),
        CreationSource::Inherited(mode) => mode,
    };
    payload.insert(
        PLANNER_PERMISSION_MODE_PAYLOAD_KEY.to_owned(),
        serde_json::json!(mode),
    );
}

/// Whether a card has a permission mode: exactly the cards [`PlannerBinding::from_card`] binds as
/// a Planner. PlainChat and Assistant conversations never ask.
pub(crate) fn card_has_permission_mode(card: &Card, role: CardRole) -> bool {
    PlannerBinding::from_card(card, role)
        .is_some_and(|binding| binding.profile == HarnessProfile::Planner)
}

/// A stored `permission_mode` that is not one of the modes, or no stored value at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MalformedPermissionMode {
    /// What the payload holds under the key; `None` when the key is absent.
    pub found: Option<Value>,
}

impl std::fmt::Display for MalformedPermissionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.found {
            None => write!(
                f,
                "card payload has no `{PLANNER_PERMISSION_MODE_PAYLOAD_KEY}`"
            ),
            Some(value) => write!(
                f,
                "card payload key `{PLANNER_PERMISSION_MODE_PAYLOAD_KEY}` is {value}, which is not a permission mode"
            ),
        }
    }
}

/// Read a card payload's stored mode. An absent key is malformed too: every Planner card is
/// created with a mode, and the backfill migration stamped the ones created before it.
pub fn read(payload: &Value) -> Result<PlannerPermissionMode, MalformedPermissionMode> {
    let stored = payload
        .get(PLANNER_PERMISSION_MODE_PAYLOAD_KEY)
        .ok_or(MalformedPermissionMode { found: None })?;
    serde_json::from_value(stored.clone()).map_err(|_| MalformedPermissionMode {
        found: Some(stored.clone()),
    })
}

/// Inherit the Track's current permission choice, just as delegated creation does.
/// Read and overwrite under the switch transaction, after its carrier CAS.
/// The caller rolls back this write if any later switch check refuses.
pub(crate) async fn inherit_for_switch_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    outgoing: &str,
    incoming: &str,
) -> Result<Card, InheritanceError> {
    let sqlx::types::Json(payload): sqlx::types::Json<Value> =
        sqlx::query_scalar("SELECT payload FROM cards WHERE id=?1")
            .bind(outgoing)
            .fetch_one(&mut **tx)
            .await
            .map_err(calm_truth::TruthError::from)?;
    let mode = read(&payload).map_err(InheritanceError::Malformed)?;
    Ok(crate::db::sqlite::planner_permission_mode_set_tx(tx, incoming, mode, |_| Ok(())).await?)
}

pub(crate) enum InheritanceError {
    Malformed(MalformedPermissionMode),
    Storage(calm_truth::TruthError),
}
impl From<calm_truth::TruthError> for InheritanceError {
    fn from(error: calm_truth::TruthError) -> Self {
        Self::Storage(error)
    }
}

/// The mode a Planner card shows: its stored value, or `never` when that cannot be read. Fails
/// closed, so an unreadable value never reads as permission to ask; the stored value is kept as
/// it is (it is sticky), and choosing a mode overwrites it.
pub(crate) fn shown_for_planner(card: &Card) -> PlannerPermissionMode {
    read(&card.payload).unwrap_or_else(|malformed| {
        tracing::warn!(
            card_id = %card.id,
            error = %malformed,
            "planner permission mode unreadable; shown as never"
        );
        PlannerPermissionMode::Never
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_stored_mode_reads_back_and_anything_else_is_malformed() {
        assert_eq!(
            read(&json!({"permission_mode": "never"})),
            Ok(PlannerPermissionMode::Never)
        );
        assert_eq!(
            read(&json!({"permission_mode": "ask"})),
            Ok(PlannerPermissionMode::Ask)
        );
        assert_eq!(
            read(&json!({"permission_mode": "full"})),
            Ok(PlannerPermissionMode::Full)
        );
        assert_eq!(
            read(&json!({})),
            Err(MalformedPermissionMode { found: None })
        );
        for corrupt in [
            json!("yolo"),
            json!("Ask"),
            json!("Full"),
            json!(null),
            json!(true),
            json!({}),
            json!({"ask": null}),
            json!({"never": null}),
            json!({"full": null}),
        ] {
            assert_eq!(
                read(&json!({"permission_mode": corrupt.clone()})),
                Err(MalformedPermissionMode {
                    found: Some(corrupt)
                })
            );
        }
    }

    #[test]
    fn creation_overwrites_whatever_the_payload_already_said() {
        let mut payload = Map::new();
        payload.insert("permission_mode".into(), json!("ask"));
        mint_at_creation(
            &mut payload,
            CreationSource::Independent,
            &AgentProvider::Codex,
            &AcpPlannerHost::unconfigured_scratch().unwrap(),
        );
        assert_eq!(payload["permission_mode"], json!("never"));
    }

    #[test]
    fn the_shared_planner_payload_mints_never_for_every_provider() {
        use crate::session_projection_repo::AgentProvider;
        for provider in AgentProvider::ALL {
            let payload = crate::track_creation::planner_harness_card_payload(
                &crate::acp_planner::config::AcpPlannerHost::unconfigured_scratch().unwrap(),
                Some("goal".into()),
                provider,
            );
            assert_eq!(payload["permission_mode"], json!("never"), "{payload}");
        }
    }

    #[test]
    fn the_shared_payload_resolves_only_the_selected_registration() {
        use crate::acp_planner::config::AcpPlannerConfig;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("acp.json");
        std::fs::write(&path, json!({"agents":[{"provider":"opencode","command":"/unused/opencode","args":[],"env":{},"min_agent_version":"1.0.0","default_permission_mode":"full"}]}).to_string()).unwrap();
        let host = AcpPlannerHost::new(
            Some(AcpPlannerConfig::read(&path).unwrap()),
            root.path(),
            root.path().join("shim"),
            root.path().join("sock"),
        )
        .unwrap();
        for provider in AgentProvider::ALL {
            let expected = if provider == AgentProvider::OpenCode {
                "full"
            } else {
                "never"
            };
            let payload =
                crate::track_creation::planner_harness_card_payload(&host, None, provider);
            assert_eq!(payload["permission_mode"], json!(expected));
        }
    }

    fn card(payload: Value) -> Card {
        Card {
            id: "card".into(),
            track_id: "track".into(),
            kind: "codex".into(),
            title: None,
            sort: 0.0,
            payload,
            runtime: None,
            deletable: false,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn only_a_planner_conversation_has_a_permission_mode() {
        let planner = card(json!({"planner_provider": "claude"}));
        assert!(card_has_permission_mode(&planner, CardRole::Planner));
        let assistant = card(json!({"harness_profile": "assistant"}));
        assert!(!card_has_permission_mode(&assistant, CardRole::Assistant));
        let plain_chat = card(json!({"harness_profile": "plain_chat"}));
        assert!(!card_has_permission_mode(&plain_chat, CardRole::Worker));
        let unbound = card(json!({"planner_provider": "gpt"}));
        assert!(!card_has_permission_mode(&unbound, CardRole::Planner));
    }
}

#[cfg(test)]
mod migration_tests;
