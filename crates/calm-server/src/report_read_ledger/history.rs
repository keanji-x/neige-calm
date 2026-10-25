//! Report owns the correspondence between rendered blocks and historical card payloads.
use super::{HistoryRef, HistoryUnavailable, LastRead};
use crate::error::CalmError;
use crate::ids::TrackId;
use crate::track_report::{ReportBlock, TrackReportPayload};
use sqlx::{Sqlite, Transaction};

async fn payload_at_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &TrackId,
    report: &str,
    commit: &str,
) -> Result<Option<TrackReportPayload>, CalmError> {
    let bytes = calm_truth::track_vcs::blob_at_in_tx(
        tx,
        track,
        commit,
        &format!("cards/{report}/.payload.json"),
    )
    .await?;
    // Old or malformed historical payloads cannot locate knowledge, but do not undo a read.
    Ok(bytes.and_then(|bytes| serde_json::from_slice(&bytes).ok()))
}

/// Only rendered blocks whose complete identity and content match this historical snapshot
/// acquire a reference. Untouched knowledge keeps its own (possibly different) commit.
pub(crate) async fn locate_read_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &TrackId,
    report: &str,
    rendered: &[String],
    snapshot: &[ReportBlock],
    read: &mut LastRead,
) -> Result<(), CalmError> {
    let head = calm_truth::track_vcs::head_in_tx(tx, track).await?;
    let payload = match head.as_deref() {
        Some(commit) => payload_at_tx(tx, track, report, commit).await?,
        None => None,
    };
    for block in snapshot.iter().filter(|block| rendered.contains(&block.id)) {
        let located = payload
            .as_ref()
            .and_then(|payload| payload.blocks.as_ref())
            .is_some_and(|blocks| {
                let mut matching = blocks.iter().filter(|old| old.id == block.id);
                matching.next() == Some(block) && matching.next().is_none()
            });
        let history_ref = if located {
            HistoryRef::Commit(head.clone().expect("payload requires a HEAD"))
        } else {
            HistoryRef::Unavailable(HistoryUnavailable::NotLocated)
        };
        read.blocks
            .get_mut(&block.id)
            .expect("rendered block recorded")
            .history_ref = history_ref;
    }
    Ok(())
}

/// Called after the generic event/VCS steps and before SQL commit. A failed location here is
/// a broken write invariant, so abort the entire transaction rather than certify other content.
pub(crate) async fn locate_written_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &TrackId,
    report: &str,
    commit: &str,
    final_payload: &TrackReportPayload,
    written: &[String],
    read: &mut LastRead,
) -> Result<(), CalmError> {
    let payload = payload_at_tx(tx, track, report, commit).await?;
    if payload.as_ref() != Some(final_payload) {
        return Err(CalmError::Internal(
            "Report history does not match final written payload".into(),
        ));
    }
    let blocks = final_payload.blocks.as_ref().ok_or_else(|| {
        CalmError::Internal("Report final written payload has no block snapshot".into())
    })?;
    for block in blocks.iter().filter(|block| written.contains(&block.id)) {
        let known = read.blocks.get_mut(&block.id).ok_or_else(|| {
            CalmError::Internal("Report written block is absent from knowledge".into())
        })?;
        if known.rev != block.rev {
            return Err(CalmError::Internal(
                "Report written block knowledge has a different revision".into(),
            ));
        }
        known.history_ref = HistoryRef::Commit(commit.to_owned());
    }
    Ok(())
}

/// Resolve only the exact stable id/revision. The commit's author is not evidence of authorship.
pub(crate) async fn block_at_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &TrackId,
    report: &str,
    commit: &str,
    id: &str,
    rev: u32,
) -> Result<Option<ReportBlock>, CalmError> {
    let Some(payload) = payload_at_tx(tx, track, report, commit).await? else {
        return Ok(None);
    };
    let Some(blocks) = payload.blocks else {
        return Ok(None);
    };
    let mut matches = blocks.into_iter().filter(|b| b.id == id);
    let block = matches.next();
    Ok(block.filter(|b| b.rev == rev && matches.next().is_none()))
}

/// A full-read certificate can prove a new id was absent only when its historical document
/// is located too. Partial knowledge and missing history never stand in for an empty before.
pub(crate) async fn whole_snapshot_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &TrackId,
    report: &str,
    read: &LastRead,
) -> Result<Option<Vec<ReportBlock>>, CalmError> {
    if !read.whole {
        return Ok(None);
    }
    let commits = read
        .blocks
        .values()
        .filter_map(|block| match &block.history_ref {
            HistoryRef::Commit(commit) => Some(commit),
            HistoryRef::Unavailable(_) => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    for commit in commits {
        let Some(payload) = payload_at_tx(tx, track, report, commit).await? else {
            continue;
        };
        if payload.doc_rev != read.doc_rev {
            continue;
        }
        let Some(blocks) = payload.blocks else {
            continue;
        };
        let indexed = blocks
            .iter()
            .map(|block| (block.id.as_str(), block.rev))
            .collect::<std::collections::BTreeMap<_, _>>();
        if indexed.len() == blocks.len()
            && read
                .blocks
                .iter()
                .all(|(id, known)| indexed.get(id.as_str()) == Some(&known.rev))
        {
            return Ok(Some(blocks));
        }
    }
    Ok(None)
}
