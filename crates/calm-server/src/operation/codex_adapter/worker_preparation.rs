//! Provider-owned atomic worker card/session publication from the shared checkout plan.
use super::*;

#[async_trait]
impl crate::operation::workspace_lease::worker::preparation::WorkerAdmission
    for CodexWorkerAdapter
{
    async fn publish_worker(
        &self,
        tx: &mut Tx<'_>,
        op: &Operation,
        plan: crate::operation::workspace_lease::WorkerLeasePlan,
    ) -> Result<TxOutput> {
        let input = &op.payload;
        let payload: CodexWorkerOperationPayload = serde_json::from_value(input.clone())?;
        crate::operation::refuse_if_context_stale(tx, Some(&payload.idempotency_key)).await?;
        // Derived from the `tasks` row inside this tx (never carried on the payload, which would move `stable_payload_hash`); `None` leaves the card untitled.
        let card_title =
            crate::operation::task_key_for_card_title(tx, &payload.idempotency_key).await;
        let card_id = new_id();
        let runtime_id = new_id();
        let track_id = TrackId::from(payload.track_id.clone());
        let cwd = plan.path.to_string_lossy().to_string();
        let env = build_codex_env(self.repo.as_ref(), self.codex.as_ref(), &card_id).await?;
        let rendered_prompt = render_task_worker_prompt_tx(
            tx,
            &payload.idempotency_key,
            &payload.goal,
            &payload.context,
            payload.acceptance_criteria.as_deref(),
            &plan,
            self.gate_run_wait,
        )
        .await?;
        let scope = card_scope(
            self.repo.as_ref(),
            CardId::from(card_id.clone()),
            track_id.clone(),
        )
        .await?;

        let (mut card, term, _mcp_token) = card_with_codex_create_tx(
            tx,
            card_id.clone(),
            &runtime_id,
            Some(op.id.as_str()),
            track_id,
            None,
            None,
            cwd.clone(),
            env.clone(),
            None,
            None,
            None,
            CardRole::Worker,
            true,
            &self.card_role_cache,
            RequestTheme::default_dark(),
        )
        .await?;

        let (lease, lease_event) =
            acquire_workspace_lease_tx(tx, &card_id, card.track_id.as_str(), &op.id, &plan).await?;

        if let Some(existing_map) = card.payload.as_object() {
            let mut merged = existing_map.clone();
            merged.insert(
                "idempotency_key".into(),
                Value::String(payload.idempotency_key.clone()),
            );
            merged.insert("role_request".into(), Value::String("codex".into()));
            merged.insert("goal".into(), Value::String(payload.goal.clone()));
            merged.insert("context".into(), payload.context.clone());
            if let Some(ac) = payload.acceptance_criteria.as_ref() {
                merged.insert("acceptance_criteria".into(), Value::String(ac.clone()));
            }
            merged.insert("prompt".into(), Value::String(rendered_prompt.clone()));
            card = card_update_tx(
                tx,
                card.id.as_ref(),
                crate::model::CardPatch {
                    title: card_title,
                    kind: None,
                    sort: None,
                    payload: Some(Value::Object(merged)),
                    deletable: None,
                },
            )
            .await?;
        }

        let mut output = TxOutput::new(
            "card",
            Some(card.id.to_string()),
            serde_json::to_value(&card)?,
        );
        output.data = json!({
            "card_id": card.id,
            "runtime_id": runtime_id,
            "track_id": card.track_id,
            "terminal_id": term.id,
            "cwd": cwd,
            "lease_id": lease.lease_id,
            "branch": plan.branch,
            "base_sha": plan.base.base_sha,
            "canonical_path": plan.base.canonical_path,
            "terminal_launch": crate::operation::terminal_launch::fresh_state(),
            "runtime_started_event_persisted": false,
            "env": env,
            "prompt": rendered_prompt,
            "scope": scope,
        });
        record_declared_head(&mut output.data, plan.declared_head());
        record_catch_up(&mut output.data, plan.catch_up.as_ref())?;
        output.post_commit_events.extend(plan.superseded);
        output.post_commit_events.push(lease_event);
        Ok(output)
    }
}
