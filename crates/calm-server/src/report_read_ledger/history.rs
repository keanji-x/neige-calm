//! Report owns the correspondence between rendered blocks and historical card payloads.
use super::{HistoryRef, HistoryUnavailable, LastRead};
use crate::error::CalmError;
use crate::ids::TrackId;
use crate::track_report::{ReportBlock, TrackReportPayload};
use sqlx::{Sqlite, Transaction};

pub(crate) async fn payload_at_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &TrackId,
    report: &str,
    commit: &str,
) -> Result<Option<TrackReportPayload>, CalmError> {
    #[cfg(test)]
    probe::loaded(commit);
    let bytes = calm_truth::track_vcs::blob_at_in_tx(
        tx,
        track,
        commit,
        &format!("cards/{report}/.payload.json"),
    )
    .await?;
    #[cfg(test)]
    if bytes.is_some() {
        probe::parsed(commit);
    }
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

/// Resolve only the exact stable id/revision from an already loaded exact-commit payload.
pub(super) fn block_at<'a>(
    payload: &'a TrackReportPayload,
    id: &str,
    rev: u32,
) -> Option<&'a ReportBlock> {
    let blocks = payload.blocks.as_ref()?;
    let mut matches = blocks.iter().filter(|b| b.id == id);
    matches
        .next()
        .filter(|b| b.rev == rev && matches.next().is_none())
}

/// A full-read certificate can prove absence only against a complete located document.
pub(super) fn whole_ids(
    payload: &TrackReportPayload,
    read: &LastRead,
) -> Option<std::collections::BTreeSet<String>> {
    if !read.whole || payload.doc_rev != read.doc_rev {
        return None;
    }
    let blocks = payload.blocks.as_ref()?;
    let indexed = blocks
        .iter()
        .map(|block| (block.id.as_str(), block.rev))
        .collect::<std::collections::BTreeMap<_, _>>();
    (indexed.len() == blocks.len()
        && read
            .blocks
            .iter()
            .all(|(id, known)| indexed.get(id.as_str()) == Some(&known.rev)))
    .then(|| indexed.into_keys().map(str::to_owned).collect())
}

#[cfg(test)]
pub(crate) mod probe {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};
    static LOADS: LazyLock<Mutex<HashMap<String, usize>>> = LazyLock::new(Default::default);
    static PARSES: LazyLock<Mutex<HashMap<String, usize>>> = LazyLock::new(Default::default);
    pub(super) fn loaded(commit: &str) {
        *LOADS.lock().unwrap().entry(commit.to_owned()).or_default() += 1;
    }
    pub(super) fn parsed(commit: &str) {
        *PARSES.lock().unwrap().entry(commit.to_owned()).or_default() += 1;
    }
    pub(crate) fn counts() -> HashMap<String, usize> {
        LOADS.lock().unwrap().clone()
    }
    pub(crate) fn parses() -> HashMap<String, usize> {
        PARSES.lock().unwrap().clone()
    }
    pub(crate) fn reset() {
        LOADS.lock().unwrap().clear();
        PARSES.lock().unwrap().clear();
    }
}
