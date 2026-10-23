//! #2599: a completion observation names the result contract its task declared, read from the
//! tasks row, so the Planner receipt can lead with the contract's conclusion. Live push and boot
//! replay share this one enrichment. No row, or a row on another track, leaves the observation
//! untyped, the receipt a legacy worker's completion gets.
use crate::db::RepoRead;
use crate::harness::Observation;
use crate::ids::TrackId;

pub(super) async fn attach_result_contract(
    repo: &dyn RepoRead,
    track_id: &TrackId,
    observation: &mut Observation,
) -> crate::error::Result<()> {
    let Observation::TaskCompleted {
        idempotency_key,
        result_contract,
        ..
    } = observation
    else {
        return Ok(());
    };
    if let Some(task) = calm_truth::db::RepoRead::task_get(repo, idempotency_key).await?
        && task.track_id == track_id.as_str()
    {
        *result_contract = task.result_contract;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::resolve_harness_observation;
    use crate::event::Event;
    use crate::harness::Observation;
    use crate::ids::TrackId;

    fn completed(attempt: &str) -> Event {
        Event::TaskCompleted {
            notification: calm_types::event::PlannerNotification::Notify,
            idempotency_key: attempt.into(),
            result: serde_json::json!({"verdict": "pass"}),
            artifacts: Vec::new(),
            agent_message: None,
        }
    }

    #[tokio::test]
    async fn a_completion_observation_carries_its_tasks_result_contract() {
        let repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap();
        crate::db::write_in_tx_typed(&repo, |tx| {
            Box::pin(async move {
                for (id, contract) in [("typed", Some("review-v1")), ("untyped", None)] {
                    sqlx::query(
                        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,\
                         depends_on_json,priority,status,result_contract,created_at_ms,\
                         updated_at_ms) VALUES(?1,'w',?1,'claude','g','{}','[]',0,'done',?2,1,1)",
                    )
                    .bind(id)
                    .bind(contract)
                    .execute(&mut **tx)
                    .await?;
                }
                Ok(())
            })
        })
        .await
        .unwrap();
        let contract_of = async |track: &str, attempt: &str| match resolve_harness_observation(
            &repo,
            &TrackId::from(track),
            &completed(attempt),
        )
        .await
        .unwrap()
        {
            Some(Observation::TaskCompleted {
                result_contract, ..
            }) => result_contract,
            other => panic!("completion observation required, got {other:?}"),
        };
        assert_eq!(
            contract_of("w", "typed").await.as_deref(),
            Some("review-v1")
        );
        assert_eq!(contract_of("w", "untyped").await, None);
        assert_eq!(contract_of("w", "rowless").await, None);
        assert_eq!(contract_of("foreign", "typed").await, None);
    }
}
