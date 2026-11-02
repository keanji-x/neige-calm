//! Owned execution locations, including removals whose live runtime may have disappeared.
use crate::error::{CalmError, Result};
use std::path::PathBuf;
use workspace::execution::ExecutionId;

pub struct OwnedWorkspaceLocation {
    pub track_id: String,
    pub path: PathBuf,
    /// A track lock cannot stand in for durable settlement after a lost wait or restart.
    pub removing: bool,
    pub retired: bool,
}

/// The durable part of the shared owned/teardown target set. Callers append their live
/// teardown snapshot; writer admission reads this inventory in its own transaction.
/// Reclaim freezes the target before deleting owner rows and commits its execution id
/// with execution admission. Only durable execution settlement releases that target.
pub async fn owned_workspace_locations(
    conn: &mut sqlx::SqliteConnection,
) -> Result<Vec<OwnedWorkspaceLocation>> {
    inventory(conn, true).await
}

/// Execution has already checked retirement through the path reservation owner.
/// This inventory retains only live lifecycle owners and old physical effects.
pub async fn execution_workspace_locations(
    conn: &mut sqlx::SqliteConnection,
) -> Result<Vec<OwnedWorkspaceLocation>> {
    inventory(conn, false).await
}

async fn inventory(
    conn: &mut sqlx::SqliteConnection,
    include_retired: bool,
) -> Result<Vec<OwnedWorkspaceLocation>> {
    let mut locations = super::track_worktree_locations(conn)
        .await?
        .into_iter()
        .map(|(track_id, path)| OwnedWorkspaceLocation {
            track_id,
            path,
            removing: false,
            retired: false,
        })
        .collect::<Vec<_>>();
    if include_retired {
        locations.extend(
            super::workspace_retirement::retired_location_roots(conn)
                .await?
                .into_iter()
                .map(|(track_id, path)| OwnedWorkspaceLocation {
                    track_id,
                    path,
                    removing: false,
                    retired: true,
                }),
        );
        locations.extend(deferred_reclaim_locations(conn).await?.into_iter().map(
            |(track_id, path)| OwnedWorkspaceLocation {
                track_id,
                path,
                removing: false,
                retired: true,
            },
        ));
    }
    #[derive(serde::Deserialize)]
    struct Request {
        path: PathBuf,
    }
    #[derive(serde::Deserialize)]
    struct Target {
        track_id: String,
        request: Request,
    }
    #[derive(serde::Deserialize)]
    struct Payload {
        target: Target,
    }
    #[derive(serde::Deserialize)]
    struct Reclaim {
        // Match calm-server workspace_service/reclaim/operation.rs Record:
        // explicit null is registered but not admitted; a missing key is corruption.
        // The server-private contract cannot be imported into the truth layer.
        #[serde(deserialize_with = "required_execution")]
        execution_id: Option<ExecutionId>,
    }
    fn required_execution<'de, D: serde::Deserializer<'de>>(
        d: D,
    ) -> std::result::Result<Option<ExecutionId>, D::Error> {
        serde::Deserialize::deserialize(d)
    }
    #[derive(serde::Deserialize)]
    struct Data {
        workspace_reclaim: Reclaim,
    }
    #[derive(serde::Deserialize)]
    struct Output {
        data: Data,
    }
    #[derive(serde::Deserialize)]
    struct Intent {
        consumer: String,
    }
    // Keep every operation visible, including missing links and unreadable intents.
    // The SQL join only loads the linked row; typed decoding establishes its identity.
    #[derive(sqlx::FromRow)]
    struct ReclaimRow {
        id: String,
        payload_json: String,
        tx_output_json: Option<String>,
        intent_json: Option<String>,
        settled_at_ms: Option<i64>,
    }
    let rows: Vec<ReclaimRow> = sqlx::query_as(
            "SELECT o.id,o.payload_json,o.tx_output_json,e.intent_json,e.settled_at_ms \
             FROM operations o LEFT JOIN workspace_executions e \
               ON e.execution_id = json_extract(o.tx_output_json,'$.data.workspace_reclaim.execution_id') \
             WHERE o.kind = 'workspace-reclaim'",
        )
        .fetch_all(conn)
        .await?;
    for ReclaimRow {
        id,
        payload_json: payload,
        tx_output_json: output,
        intent_json: intent,
        settled_at_ms,
    } in rows
    {
        let location = (|| -> Result<_> {
            let output: Output = serde_json::from_str(
                output
                    .as_deref()
                    .ok_or_else(|| CalmError::Conflict("missing reclaim output"))?,
            )?;
            let Some(execution_id) = output.data.workspace_reclaim.execution_id else {
                return Ok(None);
            };
            let intent = intent.ok_or_else(|| {
                CalmError::Conflict(format!(
                    "missing reclaim execution {}",
                    execution_id.as_str()
                ))
            })?;
            let intent: Intent = serde_json::from_str(&intent)?;
            if intent.consumer != "workspace-reclaim" {
                return Err(CalmError::Conflict("unexpected reclaim execution consumer"));
            }
            // Only an existing, typed link can establish durable settlement.
            if settled_at_ms.is_some() {
                return Ok(None);
            }
            let payload: Payload = serde_json::from_str(&payload)?;
            Ok(Some(OwnedWorkspaceLocation {
                track_id: payload.target.track_id,
                path: payload.target.request.path,
                removing: true,
                retired: false,
            }))
        })()
        .map_err(|error| {
            CalmError::Conflict(format!(
                "workspace reclaim {id} has an unreadable owned location: {error}"
            ))
        })?;
        locations.extend(location);
    }
    Ok(locations)
}

/// Only known unadmitted effects become retirement reservations. Already admitted
/// corruption remains the legacy owner's concern; it must not newly gate independent workers.
pub(super) async fn deferred_reclaim_locations(
    conn: &mut sqlx::SqliteConnection,
) -> Result<Vec<(String, PathBuf)>> {
    #[derive(serde::Deserialize)]
    struct Request {
        path: PathBuf,
    }
    #[derive(serde::Deserialize)]
    struct Target {
        track_id: String,
        provision_id: i64,
        request: Request,
    }
    #[derive(serde::Deserialize)]
    struct Payload {
        target: Target,
    }
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT id,payload_json FROM operations WHERE kind='workspace-reclaim' AND phase='parked' AND \
         CASE WHEN json_valid(tx_output_json) THEN json_type(tx_output_json,'$.data.workspace_reclaim.execution_id') END='null'",
    ).fetch_all(&mut *conn).await?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let mut roots = Vec::new();
    for (id, row) in rows {
        let target = match serde_json::from_str::<Payload>(&row) {
            Ok(payload) => payload.target,
            Err(error) => {
                // Explicit null proves this record admitted no destructive effect.
                // Its owner reports the malformed target during recovery; it must
                // not create a new global dependency for independent execution.
                tracing::warn!(operation_id=%id,%error,"unadmitted reclaim target unreadable; cleanup deferred");
                continue;
            }
        };
        if let Some(claim) =
            super::workspace_retirement::extra_retirement_claim(conn, target.provision_id).await?
        {
            if claim.track_id != target.track_id
                || std::path::Path::new(&claim.path) != target.request.path.as_path()
            {
                tracing::warn!(operation_id=%id,"unadmitted reclaim target mismatches receipt; cleanup deferred");
            }
            // No destructive execution exists. Reserve the authoritative receipt,
            // never the corrupt frozen target, without gating unrelated execution.
            roots.push((claim.track_id, claim.path.into()));
        }
    }
    Ok(roots)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db::sqlite::SqlxRepo, error::CalmError};
    use serde_json::{Value, json};

    async fn inventory(
        payload: Value,
        output: Option<Value>,
        consumer: Option<Value>,
        settled: bool,
    ) -> Result<Vec<OwnedWorkspaceLocation>> {
        inventory_with_intent(
            payload,
            output,
            consumer.map(|consumer| json!({"consumer": consumer})),
            settled,
            EXECUTION_ID,
        )
        .await
    }

    async fn inventory_with_intent(
        payload: Value,
        output: Option<Value>,
        intent: Option<Value>,
        settled: bool,
        execution_id: &str,
    ) -> Result<Vec<OwnedWorkspaceLocation>> {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        if let Some(intent) = intent {
            (repo.db()).writer().execute(sqlx::query("INSERT INTO workspace_executions(execution_id,track_id,alias,intent_json,secrets_json,desired,settled_at_ms,settlement_json) VALUES (?,'track','reclaim',?,'{}','run',?,?)")
                .bind(execution_id)
                .bind(intent.to_string())
                .bind(settled.then_some(1_i64))
                .bind(settled.then_some("{}"))).await.unwrap();
        }
        (repo.db()).writer().execute(sqlx::query("INSERT INTO operations(id,operation_key,kind,payload_hash,target_type,target_json,payload_json,tx_output_json,phase,created_at_ms,updated_at_ms) VALUES ('op','op','workspace-reclaim','hash','track','{}',?,?,'pending',1,1)")
            .bind(payload.to_string()).bind(output.map(|value| value.to_string()))).await.unwrap();
        owned_workspace_locations(&mut repo.pool().acquire().await.unwrap()).await
    }

    fn payload() -> Value {
        json!({"target": {"track_id": "track", "request": {"path": "/owned/root"}}})
    }
    const EXECUTION_ID: &str = "0123456789abcdef0123456789abcdef";

    fn output() -> Value {
        json!({"data": {"workspace_reclaim": {"execution_id": EXECUTION_ID}}})
    }
    async fn rejects(payload: Value, output: Option<Value>, consumer: Option<Value>) {
        assert!(matches!(
            inventory(payload, output, consumer, false).await,
            Err(CalmError::Core(calm_types::error::CoreError::Conflict(_)))
        ));
    }

    #[tokio::test]
    async fn owned_workspace_locations_object_path_conflicts() {
        let mut payload = payload();
        payload["target"]["request"]["path"] = json!({"path": "/owned/root"});
        rejects(payload, Some(output()), Some(json!("workspace-reclaim"))).await;
    }

    #[tokio::test]
    async fn owned_workspace_locations_array_path_conflicts() {
        let mut payload = payload();
        payload["target"]["request"]["path"] = json!(["/owned/root"]);
        rejects(payload, Some(output()), Some(json!("workspace-reclaim"))).await;
    }

    #[tokio::test]
    async fn owned_workspace_locations_missing_track_conflicts() {
        let mut payload = payload();
        payload["target"]
            .as_object_mut()
            .unwrap()
            .remove("track_id");
        rejects(payload, Some(output()), Some(json!("workspace-reclaim"))).await;
    }

    #[tokio::test]
    async fn owned_workspace_locations_missing_execution_link_conflicts() {
        rejects(
            payload(),
            Some(json!({"data": {"workspace_reclaim": {}}})),
            Some(json!("workspace-reclaim")),
        )
        .await;
    }

    #[tokio::test]
    async fn owned_workspace_locations_null_execution_link_is_excluded() {
        assert!(
            inventory(
                payload(),
                Some(json!({"data": {"workspace_reclaim": {"execution_id": null}}})),
                None,
                false,
            )
            .await
            .unwrap()
            .is_empty()
        );
    }

    #[tokio::test]
    async fn owned_workspace_locations_invalid_execution_link_conflicts() {
        for value in [json!(1), json!(true), json!({}), json!([])] {
            rejects(
                payload(),
                Some(json!({"data": {"workspace_reclaim": {"execution_id": value}}})),
                None,
            )
            .await;
        }
    }

    #[tokio::test]
    async fn owned_workspace_locations_malformed_target_conflicts() {
        for value in [json!({}), json!([])] {
            let mut malformed = payload();
            malformed["target"]["track_id"] = value;
            rejects(malformed, Some(output()), Some(json!("workspace-reclaim"))).await;
        }
        let mut missing = payload();
        missing["target"]["request"]
            .as_object_mut()
            .unwrap()
            .remove("path");
        rejects(missing, Some(output()), Some(json!("workspace-reclaim"))).await;
    }

    #[tokio::test]
    async fn owned_workspace_locations_missing_output_conflicts() {
        rejects(payload(), None, Some(json!("workspace-reclaim"))).await;
    }

    #[tokio::test]
    async fn owned_workspace_locations_nonexistent_execution_conflicts() {
        rejects(payload(), Some(output()), None).await;
    }

    #[tokio::test]
    async fn owned_workspace_locations_wrong_consumer_conflicts() {
        rejects(payload(), Some(output()), Some(json!("terminal"))).await;
    }

    #[tokio::test]
    async fn owned_workspace_locations_unreadable_consumer_conflicts() {
        rejects(payload(), Some(output()), Some(json!({}))).await;
    }

    #[tokio::test]
    async fn owned_workspace_locations_settled_wrong_consumer_conflicts() {
        assert!(matches!(
            inventory(payload(), Some(output()), Some(json!("terminal")), true).await,
            Err(CalmError::Core(calm_types::error::CoreError::Conflict(_)))
        ));
    }

    #[tokio::test]
    async fn owned_workspace_locations_settled_missing_consumer_conflicts() {
        assert!(matches!(
            inventory_with_intent(
                payload(),
                Some(output()),
                Some(json!({})),
                true,
                EXECUTION_ID
            )
            .await,
            Err(CalmError::Core(calm_types::error::CoreError::Conflict(_)))
        ));
    }

    #[tokio::test]
    async fn owned_workspace_locations_settled_unreadable_consumer_conflicts() {
        assert!(matches!(
            inventory(payload(), Some(output()), Some(json!({})), true).await,
            Err(CalmError::Core(calm_types::error::CoreError::Conflict(_)))
        ));
    }

    #[tokio::test]
    async fn owned_workspace_locations_invalid_execution_id_string_conflicts() {
        let mut output = output();
        output["data"]["workspace_reclaim"]["execution_id"] = json!("execution");
        assert!(matches!(
            inventory_with_intent(
                payload(),
                Some(output),
                Some(json!({"consumer": "workspace-reclaim"})),
                false,
                "execution",
            )
            .await,
            Err(CalmError::Core(calm_types::error::CoreError::Conflict(_)))
        ));
    }

    #[tokio::test]
    async fn owned_workspace_locations_unsettled_is_removing() {
        let locations = inventory(
            payload(),
            Some(output()),
            Some(json!("workspace-reclaim")),
            false,
        )
        .await
        .unwrap();
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].track_id, "track");
        assert_eq!(locations[0].path, PathBuf::from("/owned/root"));
        assert!(locations[0].removing);
    }

    #[tokio::test]
    async fn owned_workspace_locations_settled_is_excluded() {
        assert!(
            inventory(
                payload(),
                Some(output()),
                Some(json!("workspace-reclaim")),
                true
            )
            .await
            .unwrap()
            .is_empty()
        );
    }
}
