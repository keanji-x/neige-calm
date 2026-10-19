//! Server-built, bounded context; transcript content is evidence, never authorization.
use super::handoff_cutover::ValidatedCutoverPlan;
use crate::db::Repo;
use crate::error::{CalmError, Result};

const HISTORY_LIMIT: usize = 12_000;
const TOTAL_LIMIT: usize = 32_768;

fn excerpt(text: &str, budget: usize) -> String {
    let mut chars = text.chars();
    let text: String = chars.by_ref().take(budget.saturating_sub(80)).collect();
    if chars.next().is_some() {
        format!(
            include_str!("../../prompts/handoff-activation/truncation.md"),
            text = text
        )
    } else {
        text
    }
}

pub(super) async fn render(repo: &dyn Repo, plan: &ValidatedCutoverPlan) -> Result<String> {
    let s = &plan.scope;
    let mut text = format!(
        include_str!("../../prompts/handoff-activation/header.md"),
        s.plan_id,
        s.source_card_id,
        s.source_session_id,
        s.target_card_id,
        s.target_session_id,
        s.new_session_id
    );
    let cards = repo.cards_by_track(s.track_id.as_str()).await?;
    if cards.iter().any(|card| card.kind == "track-report") {
        let (_, card, payload) =
            crate::track_report::resolve_report_for_track(repo, s.track_id.as_str()).await?;
        let report = calm_truth::track_fs_view::report_markdown(
            card.id.as_str(),
            serde_json::to_value(payload)?,
        )
        .map_err(|error| CalmError::Internal(format!("{error:?}")))?;
        text.push_str(&format!(
            include_str!("../../prompts/handoff-activation/report.md"),
            card.id,
            excerpt(&report.content, 7000)
        ));
    } else {
        text.push_str(include_str!(
            "../../prompts/handoff-activation/report-absent.md"
        ));
    }
    let tasks = repo.tasks_by_track(s.track_id.as_str()).await?;
    let mut task_text = String::new();
    for task in tasks {
        task_text.push_str(&format!(
            include_str!("../../prompts/handoff-activation/task.md"),
            task.key, task.id, task.status, task.worker_card_id
        ));
    }
    text.push_str(&format!(
        include_str!("../../prompts/handoff-activation/tasks.md"),
        excerpt(&task_text, 4000)
    ));
    if let Some(context) = &plan.configuration.conversation.template_context {
        text.push_str(&format!(
            include_str!("../../prompts/handoff-activation/template.md"),
            excerpt(&serde_json::to_string(context)?, 4000)
        ));
    }
    // One bounded newest page per source. Repository owns selection and order; no private
    // provider files, VFS-session reconstruction or guessed native session paths.
    let mut history_budget = HISTORY_LIMIT;
    for (card, session) in [
        (&s.source_card_id, &s.source_session_id),
        (&s.target_card_id, &s.target_session_id),
    ] {
        let rows = repo
            .harness_item_list_transcript_by_card(card.as_str(), 0, 64, true)
            .await?;
        let mut history = String::new();
        for row in rows
            .into_iter()
            .filter(|row| row.worker_session_id == *session)
        {
            if !matches!(
                row.item_type.as_deref(),
                Some("userMessage" | "agentMessage")
            ) || row.method != "item/completed"
            {
                continue;
            }
            let params: serde_json::Value = serde_json::from_str(&row.params)?;
            history.push_str(&format!(
                include_str!("../../prompts/handoff-activation/transcript.md"),
                card,
                session,
                row.id,
                row.item_type.as_deref().unwrap_or_default(),
                serde_json::to_string(&params)?
            ));
        }
        let part = excerpt(&history, history_budget.min(HISTORY_LIMIT / 2));
        history_budget = history_budget.saturating_sub(part.chars().count());
        text.push_str(&format!(
            include_str!("../../prompts/handoff-activation/history.md"),
            card = card,
            session = session,
            part = part
        ));
    }
    if text.chars().count() > TOTAL_LIMIT {
        return Err(CalmError::Internal(
            "activation briefing exceeds total Unicode character budget".into(),
        ));
    }
    Ok(text)
}
