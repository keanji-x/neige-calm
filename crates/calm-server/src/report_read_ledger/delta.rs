//! Read-only Report-owned suggestions. These never certify knowledge.
use super::HistoryRef;
use crate::error::{CalmError, Result};
use crate::ids::TrackId;
use crate::track_report::ReportBlock;
use calm_types::observation::{REPORT_EDITED_DATA_LINE, receipt::escape_json};
use calm_types::report_edit_diff::{MAX_BLOCK_BYTES, MAX_BLOCK_LINES, MAX_TOTAL_BYTES};
use serde_json::json;
use sqlx::{Sqlite, Transaction};

/// Both row/CRDT and ledger come from the caller's single transaction, without a HEAD shortcut.
pub(crate) async fn render_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &TrackId,
    session: &str,
    report: &str,
) -> Result<String> {
    let (actual, updated, payload, crdt): (String, i64, String, Option<Vec<u8>>) =
        sqlx::query_as("SELECT track_id,updated_at,payload,body_crdt FROM cards WHERE id=? AND kind='track-report'")
            .bind(report).fetch_one(&mut **tx).await?;
    if actual != track.as_str() {
        return Err(CalmError::Forbidden(
            "Report suggestion belongs to another track".into(),
        ));
    }
    let current = crate::track_report_read::report_doc_snapshot(
        report,
        updated,
        serde_json::from_str(&payload)?,
        crdt.as_deref(),
    )?;
    let known = crate::db::sqlite::report_knowledge::load_tx(tx, session, report).await?;
    let mut text =
        String::from("Report knowledge delta (suggestion only; does not record a read).\n");
    let structure = super::certified_sections(&current.blocks, |_| true);
    let structure_known = known
        .as_ref()
        .is_some_and(|k| k.whole && k.doc_rev == current.doc_rev && k.sections == structure);
    text.push_str(&escape_json(
        &json!({
            "doc_rev": {"known": known.as_ref().map(|k| k.doc_rev), "current": current.doc_rev},
            "structure_known": structure_known,
        })
        .to_string(),
    ));
    text.push('\n');
    let revisions = super::changed_revisions(
        known
            .as_ref()
            .into_iter()
            .flat_map(|k| k.blocks.iter())
            .map(|(id, b)| (id.clone(), b.rev))
            .collect(),
        &current.blocks.iter().collect::<Vec<_>>(),
    );
    let current_by_id: std::collections::HashMap<_, _> =
        current.blocks.iter().map(|b| (b.id.as_str(), b)).collect();
    let changes: Vec<_> = revisions
        .iter()
        .map(|change| {
            (
                change.id.as_str(),
                known.as_ref().and_then(|k| k.blocks.get(&change.id)),
                current_by_id.get(change.id.as_str()).copied(),
            )
        })
        .collect();
    if changes.is_empty() && structure_known {
        return Ok(String::new());
    }
    let prior_whole = match known.as_ref() {
        Some(read)
            if changes.iter().any(|(_, old, new)| {
                old.is_none()
                    && new.is_some_and(|block| block.kind != calm_types::report_blocks::KIND_TASK)
            }) =>
        {
            super::history::whole_snapshot_tx(tx, track, report, read).await?
        }
        _ => None,
    };
    // References are never truncated. Reserve their bytes before optional complete block diffs.
    let mut excerpts = Vec::new();
    for (id, old, new) in changes {
        let historical = match old.map(|k| &k.history_ref) {
            Some(HistoryRef::Commit(commit)) => {
                super::history::block_at_tx(tx, track, report, commit, id, old.unwrap().rev).await?
            }
            _ => None,
        };
        let addition = old.is_none()
            && new.is_some_and(|block| block.kind != calm_types::report_blocks::KIND_TASK)
            && prior_whole
                .as_ref()
                .is_some_and(|blocks| blocks.iter().all(|block| block.id != id));
        let unread_task = new
            .is_some_and(|block| block.kind == calm_types::report_blocks::KIND_TASK)
            && !historical
                .as_ref()
                .is_some_and(|block| block.kind == calm_types::report_blocks::KIND_TASK);
        let reason = if addition {
            "added"
        } else if old.is_some() && historical.is_none() {
            "history unavailable"
        } else if unread_task {
            "unread task"
        } else if old.is_none() {
            "unread"
        } else {
            "changed"
        };
        text.push_str(&escape_json(
            &json!({
                "id": id, "known_rev": old.map(|k| k.rev), "current_rev": new.map(|b| b.rev),
                "state": reason,
                "read": new.map(|_| json!({"path": "report.md", "blocks": [id]})),
                "history_ref": old.map(|known| &known.history_ref),
            })
            .to_string(),
        ));
        text.push('\n');
        // Never render an unknown task's payload. A missing historical block is not an empty before.
        if !unread_task
            && (historical.is_some() || addition)
            && let Some(diff) = complete_diff(id, historical.as_ref(), new)
        {
            excerpts.push(diff);
        }
    }
    let mut used = text.len();
    for excerpt in excerpts {
        if used + excerpt.len() <= MAX_TOTAL_BYTES {
            text.push_str(&excerpt);
            used += excerpt.len();
        }
    }
    Ok(text)
}

fn complete_diff(
    id: &str,
    before: Option<&ReportBlock>,
    after: Option<&ReportBlock>,
) -> Option<String> {
    let before = before
        .map(calm_types::report_blocks::flat_text)
        .unwrap_or_default();
    let after = after
        .map(calm_types::report_blocks::flat_text)
        .unwrap_or_default();
    let (patch, truncated) =
        calm_truth::track_vcs::unified_patch(id, &before, &after, MAX_BLOCK_LINES);
    if truncated {
        return None;
    }
    // JSON quoting reuses the receipt's data escaping and cannot close the data framing.
    let data = escape_json(&json!({"block_diff": {"id": id, "patch": patch}}).to_string());
    let text = format!("{REPORT_EDITED_DATA_LINE}{data}\n");
    (text.len() <= MAX_BLOCK_BYTES).then_some(text)
}

pub(crate) async fn attach_tx(
    tx: &mut Transaction<'_, Sqlite>,
    session: &str,
    event: &crate::event::Event,
    observation: &mut crate::harness::Observation,
) -> Result<()> {
    if let (
        crate::event::Event::TrackReportEdited {
            track_id, card_id, ..
        },
        crate::harness::Observation::ReportEdited { presentation, .. },
    ) = (event, observation)
    {
        *presentation = calm_types::observation::ReportEditPresentation::Knowledge {
            knowledge_text: render_tx(tx, track_id, session, card_id.as_str()).await?,
        };
    }
    Ok(())
}

pub(crate) async fn attach(
    repo: &dyn crate::db::RepoEventWrite,
    session: &str,
    event: &crate::event::Event,
    observation: crate::harness::Observation,
) -> Result<crate::harness::Observation> {
    if !matches!(event, crate::event::Event::TrackReportEdited { .. }) {
        return Ok(observation);
    }
    let (session, event) = (session.to_owned(), event.clone());
    crate::db::write_in_tx_typed(repo, move |tx| {
        let (session, event, mut observation) =
            (session.clone(), event.clone(), observation.clone());
        Box::pin(async move {
            attach_tx(tx, &session, &event, &mut observation).await?;
            Ok(observation)
        })
    })
    .await
}

pub(crate) async fn for_track(
    repo: &dyn crate::db::RepoEventWrite,
    track: &TrackId,
    session: &str,
) -> Result<String> {
    let (track, session) = (track.clone(), session.to_owned());
    crate::db::write_in_tx_typed(repo, move |tx| {
        let (track, session) = (track.clone(), session.clone());
        Box::pin(async move { for_track_tx(tx, &track, &session).await })
    })
    .await
}

pub(crate) async fn for_track_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &TrackId,
    session: &str,
) -> Result<String> {
    let report: Option<String> =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id=? AND kind='track-report'")
            .bind(track.as_str())
            .fetch_optional(&mut **tx)
            .await?;
    match report {
        Some(report) => render_tx(tx, track, session, &report).await,
        None => Ok(String::new()),
    }
}
