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
async fn prepare_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &TrackId,
    session: &str,
    report: &str,
) -> Result<PreparedDelta> {
    let started = std::time::Instant::now();
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
    // Decode the authoritative current CRDT once here to select required history. Moving this
    // step out too would require revalidation or eagerly reading every unchanged block's history.
    let revisions = super::changed_revisions(
        known
            .as_ref()
            .into_iter()
            .flat_map(|k| k.blocks.iter())
            .map(|(id, b)| (id.clone(), b.rev))
            .collect(),
        &current.blocks.iter().collect::<Vec<_>>(),
    );

    let current_by_id: std::collections::HashMap<_, _> = current
        .blocks
        .iter()
        .map(|block| (block.id.as_str(), block))
        .collect();
    let needs_whole = revisions.iter().any(|change| {
        change.known_rev.is_none()
            && current_by_id
                .get(change.id.as_str())
                .is_some_and(|block| block.kind != calm_types::report_blocks::KIND_TASK)
    });
    // Group changed blocks by exact commit. Whole-read candidates join the same map, so
    // each version is loaded/parsed once even when it serves both kinds of evidence.
    let mut commits = std::collections::BTreeMap::<String, Vec<usize>>::new();
    if let Some(read) = &known {
        for (index, change) in revisions.iter().enumerate() {
            if let Some(super::ReadBlock {
                history_ref: HistoryRef::Commit(commit),
                ..
            }) = read.blocks.get(&change.id)
            {
                commits.entry(commit.clone()).or_default().push(index);
            }
        }
        if needs_whole && read.whole {
            for block in read.blocks.values() {
                if let HistoryRef::Commit(commit) = &block.history_ref {
                    commits.entry(commit.clone()).or_default();
                }
            }
        }
    }
    let current_ms = started.elapsed().as_secs_f64() * 1000.0;
    #[cfg(feature = "fixtures")]
    crate::test_seams::pause_point("report-delta-history", session).await;
    let history_started = std::time::Instant::now();
    let mut versions = 0;
    let mut historical = std::collections::HashMap::new();
    let mut failure: Option<(usize, CalmError)> = None;
    let mut prior_whole = None;
    let mut search_whole = needs_whole && known.as_ref().is_some_and(|read| read.whole);
    for (commit, indices) in commits {
        // Once a whole proof is found, don't load otherwise unused candidates.
        if indices.is_empty() && !search_whole {
            continue;
        }
        versions += 1;
        let payload = super::history::payload_at_tx(tx, track, report, &commit).await;
        match payload {
            Err(error) => {
                // Whole-proof search precedes every block diff in the original contract.
                if search_whole {
                    return Err(error);
                }
                let index = indices[0];
                if failure.as_ref().is_none_or(|(prior, _)| index < *prior) {
                    failure = Some((index, error));
                }
            }
            Ok(Some(payload)) => {
                if search_whole
                    && let Some(ids) =
                        super::history::whole_ids(&payload, known.as_ref().expect("whole read"))
                {
                    prior_whole = Some(ids);
                    search_whole = false;
                }
                for index in indices {
                    let change = &revisions[index];
                    let old = &known.as_ref().expect("changed known block").blocks[&change.id];
                    if let Some(before) = super::history::block_at(&payload, &change.id, old.rev) {
                        let after = current_by_id.get(change.id.as_str()).copied();
                        let unread_task = after.is_some_and(|block| {
                            block.kind == calm_types::report_blocks::KIND_TASK
                        }) && before.kind != calm_types::report_blocks::KIND_TASK;
                        historical.insert(
                            change.id.clone(),
                            HistoricalChange {
                                was_task: before.kind == calm_types::report_blocks::KIND_TASK,
                                excerpt: if unread_task {
                                    None
                                } else {
                                    complete_diff(&change.id, Some(before), after)
                                },
                            },
                        );
                    }
                }
                // The complete historical payload is dropped here. Only bounded excerpts and
                // identity facts survive; mixed baselines never retain N complete reports.
            }
            Ok(None) => {}
        }
    }
    if let Some((_, error)) = failure {
        return Err(error);
    }
    tracing::debug!(target: "calm_server::report_delta", current_ms, versions,
        history_ms = history_started.elapsed().as_secs_f64() * 1000.0,
        "Report delta snapshot prepared");
    Ok(PreparedDelta {
        current,
        known,
        revisions,
        historical,
        prior_whole,
    })
}

struct HistoricalChange {
    was_task: bool,
    // complete_diff admits only a complete excerpt within MAX_BLOCK_BYTES.
    excerpt: Option<String>,
}

struct PreparedDelta {
    current: crate::track_report_read::ReportDocSnapshot,
    known: Option<super::LastRead>,
    revisions: Vec<super::ChangedRevision>,
    historical: std::collections::HashMap<String, HistoricalChange>,
    prior_whole: Option<std::collections::BTreeSet<String>>,
}

fn render(prepared: PreparedDelta) -> Result<String> {
    let PreparedDelta {
        current,
        known,
        revisions,
        mut historical,
        prior_whole,
    } = prepared;
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
    // References are never truncated. Reserve their bytes before optional complete block diffs.
    let mut excerpts = Vec::new();
    for (id, old, new) in changes {
        let historical = historical.remove(id);
        let addition = old.is_none()
            && new.is_some_and(|block| block.kind != calm_types::report_blocks::KIND_TASK)
            && prior_whole
                .as_ref()
                .is_some_and(|blocks| !blocks.contains(id));
        let unread_task = new
            .is_some_and(|block| block.kind == calm_types::report_blocks::KIND_TASK)
            && !historical.as_ref().is_some_and(|block| block.was_task);
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
        if !unread_task {
            if let Some(excerpt) = historical.and_then(|block| block.excerpt) {
                excerpts.push(excerpt);
            } else if addition && let Some(diff) = complete_diff(id, None, new) {
                excerpts.push(diff);
            }
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

/// Atomic callers keep rendering in their transaction; independent suggestions release it first.
#[tracing::instrument(skip_all, name = "report_knowledge_delta", fields(entry = "atomic"))]
pub(crate) async fn render_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &TrackId,
    session: &str,
    report: &str,
) -> Result<String> {
    render(prepare_tx(tx, track, session, report).await?)
}

async fn render_detached(prepared: Option<PreparedDelta>, _session: &str) -> Result<String> {
    let Some(prepared) = prepared else {
        return Ok(String::new());
    };
    #[cfg(feature = "fixtures")]
    crate::test_seams::pause_point("report-delta-render", _session).await;
    // No connection or transaction crosses into this CPU-only work. Cancellation can leave
    // bounded rendering running, but cannot retain writer admission or a read snapshot.
    let span = tracing::Span::current();
    tokio::task::spawn_blocking(move || {
        span.in_scope(|| {
            let started = std::time::Instant::now();
            let text = render(prepared);
            tracing::debug!(target: "calm_server::report_delta",
            render_ms = started.elapsed().as_secs_f64() * 1000.0,
            "Report delta rendered outside writer");
            text
        })
    })
    .await
    .map_err(|error| CalmError::Internal(format!("Report delta renderer failed: {error}")))?
}

enum Target {
    Track(TrackId),
    Report(TrackId, String),
}

async fn prepare_target_tx(
    tx: &mut Transaction<'_, Sqlite>,
    target: &Target,
    session: &str,
) -> Result<Option<PreparedDelta>> {
    match target {
        Target::Track(track) => prepare_for_track_tx(tx, track, session).await,
        Target::Report(track, report) => prepare_tx(tx, track, session, report).await.map(Some),
    }
}

async fn prepare_independent(
    repo: &dyn crate::db::Repo,
    target: Target,
    session: &str,
) -> Result<Option<PreparedDelta>> {
    if let Some(db) = repo.sqlite_db()
        && let Some(mut tx) = db.try_begin_read_snapshot().await?
    {
        // One WAL read snapshot covers current CRDT, ledger, ownership and exact history.
        // Never upgrade it or call a writer from this scope. Shared-cache connections cannot
        // use this strategy: their table locks can deadlock with a multi-table writer.
        let prepared = prepare_target_tx(&mut tx, &target, session).await?;
        tx.commit().await?;
        return Ok(prepared);
    }
    let session = session.to_owned();
    crate::db::write_in_tx_typed(repo, move |tx| {
        Box::pin(async move { prepare_target_tx(tx, &target, &session).await })
    })
    .await
}

#[tracing::instrument(skip_all, name = "report_knowledge_delta", fields(entry = "attach"))]
pub(crate) async fn attach(
    repo: &dyn crate::db::Repo,
    session: &str,
    event: &crate::event::Event,
    mut observation: crate::harness::Observation,
) -> Result<crate::harness::Observation> {
    if let (
        crate::event::Event::TrackReportEdited {
            track_id, card_id, ..
        },
        crate::harness::Observation::ReportEdited { presentation, .. },
    ) = (event, &mut observation)
    {
        let prepared = prepare_independent(
            repo,
            Target::Report(track_id.clone(), card_id.to_string()),
            session,
        )
        .await?;
        *presentation = calm_types::observation::ReportEditPresentation::Knowledge {
            knowledge_text: render_detached(prepared, session).await?,
        };
    }
    Ok(observation)
}

#[tracing::instrument(skip_all, name = "report_knowledge_delta", fields(entry = "for_track"))]
pub(crate) async fn for_track(
    repo: &dyn crate::db::Repo,
    track: &TrackId,
    session: &str,
) -> Result<String> {
    let prepared = prepare_independent(repo, Target::Track(track.clone()), session).await?;
    render_detached(prepared, session).await
}

pub(crate) async fn for_track_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &TrackId,
    session: &str,
) -> Result<String> {
    match prepare_for_track_tx(tx, track, session).await? {
        Some(prepared) => render(prepared),
        None => Ok(String::new()),
    }
}

async fn prepare_for_track_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &TrackId,
    session: &str,
) -> Result<Option<PreparedDelta>> {
    let report: Option<String> =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id=? AND kind='track-report'")
            .bind(track.as_str())
            .fetch_optional(&mut **tx)
            .await?;
    match report {
        Some(report) => prepare_tx(tx, track, session, &report).await.map(Some),
        None => Ok(None),
    }
}
