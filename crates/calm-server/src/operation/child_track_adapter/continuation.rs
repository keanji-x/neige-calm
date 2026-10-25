//! New assignments to returned children preserve the child's resources and conversation.
use super::*;

impl ChildTrackAdapter {
    pub(super) async fn continue_tx(
        &self,
        tx: &mut Tx<'_>,
        payload: &ChildTrackOperationPayload,
        child_id: &str,
    ) -> Result<TxOutput> {
        let sql = format!(
            "SELECT EXISTS(SELECT 1 FROM ({}) child WHERE child.id=?1 AND child.parent_track_id=?2)",
            calm_truth::db::sqlite::child_track_handback_targets_sql(),
        );
        let eligible: bool = sqlx::query_scalar(&sql)
            .bind(child_id)
            .bind(&payload.parent_track_id)
            .fetch_one(&mut **tx)
            .await?;
        if !eligible {
            return Err(CalmError::Conflict("child-track-continue-refused".into()));
        }
        let task_key: String =
            sqlx::query_scalar("SELECT key FROM tasks WHERE id=?1 AND track_id=?2")
                .bind(&payload.task_id)
                .bind(&payload.parent_track_id)
                .fetch_one(&mut **tx)
                .await?;
        let reused_key: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE child_track_id=?1 AND track_id=?2 AND key=?3)",
        )
        .bind(child_id)
        .bind(&payload.parent_track_id)
        .bind(&task_key)
        .fetch_one(&mut **tx)
        .await?;
        if reused_key {
            return Err(CalmError::Conflict(
                "child-track-continue-requires-new-key".into(),
            ));
        }
        calm_truth::db::sqlite::task_bind_child_track_tx(
            tx,
            &payload.task_id,
            &payload.parent_track_id,
            child_id,
            now_ms(),
        )
        .await?;
        let area_id: String = sqlx::query_scalar("SELECT area_id FROM tracks WHERE id=?1")
            .bind(child_id)
            .fetch_one(&mut **tx)
            .await?;
        let actor = ActorId::KernelDispatcher;
        let scope = EventScope::Track {
            track: child_id.to_owned().into(),
            area: area_id.into(),
        };
        let event = Event::TrackWakeRequested {
            track_id: child_id.to_owned().into(),
            source: "parent_task".into(),
            key: payload.task_id.clone(),
            text: render_child_seed(payload, &task_key),
        };
        let id = append_decision_event_in_tx(tx, &actor, &scope, None, &event).await?;
        let result = json!({"child_track_id":child_id,"continued":true});
        let mut output = TxOutput::new("track", Some(child_id.to_owned()), result.clone());
        output.data = result;
        output.post_commit_events.push(BroadcastEnvelope {
            id,
            event_version: SYNC_EVENT_VERSION,
            actor,
            scope,
            event,
        });
        Ok(output)
    }
}
