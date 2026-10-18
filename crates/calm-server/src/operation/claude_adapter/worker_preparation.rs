//! Provider-owned atomic worker card/session publication from the shared checkout plan.
use super::*;

#[async_trait]
impl crate::operation::workspace_lease::worker::preparation::WorkerAdmission
    for ClaudeWorkerAdapter
{
    async fn publish_worker(
        &self,
        tx: &mut Tx<'_>,
        op: &Operation,
        plan: crate::operation::workspace_lease::WorkerLeasePlan,
    ) -> Result<TxOutput> {
        let input = &op.payload;
        let payload: ClaudeWorkerOperationPayload = serde_json::from_value(input.clone())?;
        crate::operation::refuse_if_context_stale(tx, Some(&payload.idempotency_key)).await?;
        // Derived from the `tasks` row inside this tx (never carried on the payload, which would move `stable_payload_hash`); `None` leaves the card untitled.
        let card_title =
            crate::operation::task_key_for_card_title(tx, &payload.idempotency_key).await;
        let card_id = new_id();
        let runtime_id = new_id();
        let claude_session_id = uuid::Uuid::new_v4().to_string();
        let track_id = TrackId::from(payload.track_id.clone());
        let cwd = plan.path.to_string_lossy().to_string();
        let settings_path = self
            .codex
            .claude_settings_dir
            .join(&card_id)
            .join("settings.json");
        let settings_dir = settings_path_parent(&settings_path)?;
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
        let claude_bin = crate::claude_version::resolve_claude_bin(&self.codex.claude_bin);
        let command_line = build_claude_worker_command_line(
            &claude_bin,
            &settings_path,
            &claude_session_id,
            track_id.as_str(),
            &rendered_prompt,
        )?;
        let env = build_claude_env(self.repo.as_ref(), &self.codex, &card_id).await?;
        let scope = card_scope(
            self.repo.as_ref(),
            CardId::from(card_id.clone()),
            track_id.clone(),
        )
        .await?;

        let (mut card, term) = card_with_claude_worker_create_tx(
            tx,
            card_id.clone(),
            &runtime_id,
            Some(op.id.as_str()),
            track_id,
            None,
            None,
            command_line.clone(),
            cwd.clone(),
            env.clone(),
            Some(rendered_prompt.clone()),
            None,
            None,
            settings_path.to_string_lossy().to_string(),
            claude_session_id.clone(),
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
            merged.insert("role_request".into(), Value::String("claude".into()));
            merged.insert("goal".into(), Value::String(payload.goal.clone()));
            merged.insert("context".into(), payload.context.clone());
            if let Some(ac) = payload.acceptance_criteria.as_ref() {
                merged.insert("acceptance_criteria".into(), Value::String(ac.clone()));
            }
            merged.insert("prompt".into(), Value::String(rendered_prompt.clone()));
            merged.insert(
                "claude_session_id".into(),
                Value::String(claude_session_id.clone()),
            );
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
            "settings_path": settings_path,
            "terminal_launch": crate::operation::terminal_launch::fresh_state(),
            "settings_dir": settings_dir,
            "claude_session_id": claude_session_id,
            "claude_bin": claude_bin,
            "command_line": command_line,
            "cwd": cwd,
            "lease_id": lease.lease_id,
            "branch": plan.branch,
            "base_sha": plan.base.base_sha,
            "canonical_path": plan.base.canonical_path,
            "env": env,
            "prompt": rendered_prompt,
            "scope": scope,
        });
        crate::operation::workspace_lease::worker::record_catch_up(
            &mut output.data,
            plan.catch_up.as_ref(),
        )?;
        output.post_commit_events.extend(plan.superseded);
        output.post_commit_events.push(lease_event);
        Ok(output)
    }
}
