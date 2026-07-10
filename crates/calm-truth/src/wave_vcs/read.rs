use crate::error::{CalmError, Result};
use crate::ids::{CardId, WaveId};
use similar::TextDiff;
use sqlx::SqlitePool;
use std::collections::{BTreeMap, BTreeSet};

use super::store::{
    commit_records_for_wave_pool, load_blob_bytes_pool, load_commit_record_pool,
    load_tree_object_pool, normalize_path,
};
use super::{
    CommitHash, CommitLog, CommitLogEntry, CommitRecord, DEFAULT_PATCH_MAX_LINES, DiffEntry,
    DiffStatus, FileDiff, HistoricalBlob, ManifestEntry, SinceLastTurnBlock, TreeManifest, head,
    tree_at,
};

const LOG_FILTER_SCAN_LIMIT: usize = 1000;

const ATTRIBUTION_COMMIT_BOUND: usize = 50;

pub async fn diff(
    pool: &SqlitePool,
    from: &str,
    to: &str,
    path: Option<&str>,
) -> Result<Vec<DiffEntry>> {
    let from_tree = tree_at(pool, from)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave-vcs commit {from}")))?;
    let to_tree = tree_at(pool, to)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave-vcs commit {to}")))?;
    Ok(diff_manifests(&from_tree, &to_tree, path))
}

pub async fn diff_with_patches(
    pool: &SqlitePool,
    from: &str,
    to: &str,
    path: Option<&str>,
    max_patch_lines: usize,
) -> Result<Vec<FileDiff>> {
    let from_tree = tree_at(pool, from)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave-vcs commit {from}")))?;
    let to_tree = tree_at(pool, to)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave-vcs commit {to}")))?;
    let entries = diff_manifests(&from_tree, &to_tree, path);
    file_diffs_from_entries(pool, &from_tree, &to_tree, entries, max_patch_lines).await
}

pub async fn cat_at(pool: &SqlitePool, commit_hash: &str, path: &str) -> Result<HistoricalBlob> {
    let tree = tree_at(pool, commit_hash)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave-vcs commit {commit_hash}")))?;
    let path = normalize_path(path);
    let entry = tree
        .entries
        .get(&path)
        .ok_or_else(|| CalmError::NotFound(format!("wave-vcs path {path} at {commit_hash}")))?;
    let bytes = load_blob_bytes_pool(pool, &entry.blob_hash)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave-vcs blob {}", entry.blob_hash)))?;
    let content = String::from_utf8(bytes).map_err(|e| {
        CalmError::Internal(format!(
            "wave-vcs: blob {} at {commit_hash}:{path} is not UTF-8: {e}",
            entry.blob_hash
        ))
    })?;
    Ok(HistoricalBlob {
        commit: commit_hash.to_string(),
        path,
        content,
        content_type: entry.content_type.clone(),
    })
}

pub async fn commit_record(pool: &SqlitePool, commit_hash: &str) -> Result<Option<CommitRecord>> {
    load_commit_record_pool(pool, commit_hash).await
}

pub async fn commit_belongs_to_wave(
    pool: &SqlitePool,
    wave_id: &WaveId,
    commit_hash: &str,
) -> Result<bool> {
    let Some(record) = commit_record(pool, commit_hash).await? else {
        return Ok(false);
    };
    Ok(record.wave_id == *wave_id)
}

pub async fn log(
    pool: &SqlitePool,
    wave_id: &WaveId,
    path: Option<&str>,
    limit: usize,
) -> Result<CommitLog> {
    let limit = limit.clamp(1, 200);
    let normalized = path.map(normalize_path).filter(|path| !path.is_empty());
    let scan_limit = if normalized.is_some() {
        LOG_FILTER_SCAN_LIMIT
    } else {
        limit
    };
    let records = commit_records_for_wave_pool(pool, wave_id, scan_limit.saturating_add(1)).await?;
    let fetched = records.len();
    let mut out = Vec::new();
    let mut examined = 0;
    for record in records.into_iter().take(scan_limit) {
        examined += 1;
        let changed_paths = changed_paths_for_commit(pool, &record).await?;
        if let Some(path) = normalized.as_deref()
            && !changed_paths
                .iter()
                .any(|changed| path_matches(changed, path))
        {
            continue;
        }
        out.push(CommitLogEntry {
            hash: record.hash,
            parent_hash: record.parent_hash,
            lifecycle: record.lifecycle,
            event_id: record.event_id,
            created_at: record.created_at,
            message: record.message,
            changed_paths,
        });
        if out.len() >= limit {
            break;
        }
    }
    Ok(CommitLog {
        commits: out,
        truncated: examined < fetched,
    })
}

pub async fn since_last_turn_block(
    pool: &SqlitePool,
    wave_id: &WaveId,
    last_seen_head: Option<&str>,
    current_override: Option<&CommitHash>,
    spec_card_id: Option<&CardId>,
) -> Result<SinceLastTurnBlock> {
    let Some(current) = (match current_override {
        Some(current) => Some(current.clone()),
        None => head(pool, wave_id).await?,
    }) else {
        return Ok(SinceLastTurnBlock::empty());
    };
    let Some(previous) = last_seen_head else {
        return Ok(SinceLastTurnBlock {
            current_head: Some(current),
            block: None,
        });
    };
    if previous == current {
        return Ok(SinceLastTurnBlock {
            current_head: Some(current),
            block: None,
        });
    }

    let entries = diff(pool, previous, &current, None)
        .await?
        .into_iter()
        .filter(|entry| !is_internal_observation_diff_path(&entry.path, spec_card_id))
        .collect::<Vec<_>>();
    if entries.is_empty() {
        return Ok(SinceLastTurnBlock {
            current_head: Some(current),
            block: None,
        });
    }
    let report_patch = if entries.iter().any(|entry| {
        entry.path == "report.md"
            && matches!(entry.status, DiffStatus::Added | DiffStatus::Modified)
    }) {
        diff_with_patches(
            pool,
            previous,
            &current,
            Some("report.md"),
            DEFAULT_PATCH_MAX_LINES,
        )
        .await?
        .into_iter()
        .find_map(|entry| entry.patch)
    } else {
        None
    };
    let path_authors = path_authors_since(pool, previous, &current).await?;
    let mut out = String::new();
    out.push_str(&format!(
        "## Wave state changes since your last turn (HEAD {} -> {})\n",
        short_hash(previous),
        short_hash(&current)
    ));
    for entry in entries {
        out.push_str("- ");
        out.push_str(&entry.path);
        out.push(' ');
        out.push_str(entry.status.observation_label());
        if let Some(author) = path_authors.as_ref().and_then(|authors| {
            authors
                .get(&entry.path)
                .and_then(|author| author.as_deref())
        }) {
            out.push_str(" (by ");
            out.push_str(author);
            out.push(')');
        }
        if entry.path == "report.md" && report_patch.is_some() {
            out.push_str(" (unified patch follows)");
        }
        out.push('\n');
        if entry.path == "report.md"
            && let Some(patch) = report_patch.as_deref()
        {
            let fence = markdown_code_fence_for(patch);
            out.push_str(&fence);
            out.push_str("diff\n");
            out.push_str(patch);
            if !patch.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&fence);
            out.push('\n');
        }
    }
    Ok(SinceLastTurnBlock {
        current_head: Some(current),
        block: Some(out),
    })
}

async fn path_authors_since(
    pool: &SqlitePool,
    previous: &str,
    current: &str,
) -> Result<Option<BTreeMap<String, Option<String>>>> {
    let mut records = Vec::new();
    let mut cursor = current.to_string();
    let mut reached_previous = false;

    for _ in 0..ATTRIBUTION_COMMIT_BOUND {
        let Some(record) = load_commit_record_pool(pool, &cursor).await? else {
            return Ok(None);
        };
        let Some(parent_hash) = record.parent_hash.clone() else {
            return Ok(None);
        };
        reached_previous = parent_hash == previous;
        cursor = parent_hash;
        records.push(record);
        if reached_previous {
            break;
        }
    }

    if !reached_previous {
        return Ok(None);
    }

    let mut tree_cache = BTreeMap::<String, TreeManifest>::new();
    let mut authors = BTreeMap::<String, Option<String>>::new();
    for record in records {
        let Some(parent_hash) = record.parent_hash.as_deref() else {
            return Ok(None);
        };
        let Some(tree) = load_tree_for_commit_record(pool, &record, &mut tree_cache).await? else {
            return Ok(None);
        };
        let Some(parent) = load_tree_for_commit_hash(pool, parent_hash, &mut tree_cache).await?
        else {
            return Ok(None);
        };
        for entry in diff_manifests(&parent, &tree, None) {
            authors
                .entry(entry.path)
                .or_insert_with(|| record.author.clone());
        }
    }

    Ok(Some(authors))
}

async fn load_tree_for_commit_record(
    pool: &SqlitePool,
    record: &CommitRecord,
    cache: &mut BTreeMap<String, TreeManifest>,
) -> Result<Option<TreeManifest>> {
    if let Some(tree) = cache.get(&record.hash) {
        return Ok(Some(tree.clone()));
    }
    let Some(tree) = load_tree_object_pool(pool, &record.tree_hash).await? else {
        return Ok(None);
    };
    cache.insert(record.hash.clone(), tree.clone());
    Ok(Some(tree))
}

async fn load_tree_for_commit_hash(
    pool: &SqlitePool,
    commit_hash: &str,
    cache: &mut BTreeMap<String, TreeManifest>,
) -> Result<Option<TreeManifest>> {
    if let Some(tree) = cache.get(commit_hash) {
        return Ok(Some(tree.clone()));
    }
    let Some(tree) = tree_at(pool, commit_hash).await? else {
        return Ok(None);
    };
    cache.insert(commit_hash.to_string(), tree.clone());
    Ok(Some(tree))
}

fn diff_manifests(from: &TreeManifest, to: &TreeManifest, path: Option<&str>) -> Vec<DiffEntry> {
    let normalized = path.map(normalize_path).filter(|prefix| !prefix.is_empty());
    let mut paths = BTreeSet::new();
    paths.extend(from.entries.keys().cloned());
    paths.extend(to.entries.keys().cloned());

    let mut out = Vec::new();
    for path in paths {
        if let Some(prefix) = normalized.as_deref()
            && path != prefix
            && !path.starts_with(&format!("{prefix}/"))
        {
            continue;
        }
        let old = from.entries.get(&path);
        let new = to.entries.get(&path);
        match (old, new) {
            (None, Some(new)) => out.push(DiffEntry {
                path,
                status: DiffStatus::Added,
                old_hash: None,
                new_hash: Some(new.blob_hash.clone()),
            }),
            (Some(old), None) => out.push(DiffEntry {
                path,
                status: DiffStatus::Deleted,
                old_hash: Some(old.blob_hash.clone()),
                new_hash: None,
            }),
            (Some(old), Some(new)) if old.blob_hash != new.blob_hash => out.push(DiffEntry {
                path,
                status: DiffStatus::Modified,
                old_hash: Some(old.blob_hash.clone()),
                new_hash: Some(new.blob_hash.clone()),
            }),
            _ => {}
        }
    }
    out
}

async fn file_diffs_from_entries(
    pool: &SqlitePool,
    from_tree: &TreeManifest,
    to_tree: &TreeManifest,
    entries: Vec<DiffEntry>,
    max_patch_lines: usize,
) -> Result<Vec<FileDiff>> {
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let old_entry = from_tree.entries.get(&entry.path);
        let new_entry = to_tree.entries.get(&entry.path);
        let old_content_type = old_entry.map(|entry| entry.content_type.clone());
        let new_content_type = new_entry.map(|entry| entry.content_type.clone());
        let patch = if should_render_text_patch(old_entry, new_entry) {
            let old = load_optional_text_blob(pool, old_entry).await?;
            let new = load_optional_text_blob(pool, new_entry).await?;
            match (old, new) {
                (Some(old), Some(new)) => {
                    let (patch, truncated) =
                        unified_patch(&entry.path, &old, &new, max_patch_lines);
                    Some((patch, truncated))
                }
                _ => None,
            }
        } else {
            None
        };
        let (patch, patch_truncated) = patch.unwrap_or_else(|| (String::new(), false));
        out.push(FileDiff {
            path: entry.path,
            status: entry.status,
            old_hash: entry.old_hash,
            new_hash: entry.new_hash,
            old_content_type,
            new_content_type,
            patch: if patch.is_empty() { None } else { Some(patch) },
            patch_truncated,
        });
    }
    Ok(out)
}

fn should_render_text_patch(
    old_entry: Option<&ManifestEntry>,
    new_entry: Option<&ManifestEntry>,
) -> bool {
    old_entry
        .or(new_entry)
        .map(|entry| is_text_content_type(&entry.content_type))
        .unwrap_or(false)
        && old_entry
            .map(|entry| is_text_content_type(&entry.content_type))
            .unwrap_or(true)
        && new_entry
            .map(|entry| is_text_content_type(&entry.content_type))
            .unwrap_or(true)
}

fn is_text_content_type(content_type: &str) -> bool {
    content_type.starts_with("text/")
        || matches!(
            content_type,
            "application/json" | "application/x-ndjson" | "application/ld+json"
        )
}

async fn load_optional_text_blob(
    pool: &SqlitePool,
    entry: Option<&ManifestEntry>,
) -> Result<Option<String>> {
    let Some(entry) = entry else {
        return Ok(Some(String::new()));
    };
    let Some(bytes) = load_blob_bytes_pool(pool, &entry.blob_hash).await? else {
        return Ok(None);
    };
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|e| CalmError::Internal(format!("wave-vcs: text blob is not UTF-8: {e}")))
}

fn unified_patch(path: &str, old: &str, new: &str, max_lines: usize) -> (String, bool) {
    let old_header = format!("a/{path}");
    let new_header = format!("b/{path}");
    let patch = TextDiff::from_lines(old, new)
        .unified_diff()
        .header(&old_header, &new_header)
        .to_string();
    truncate_lines(patch, max_lines)
}

fn truncate_lines(text: String, max_lines: usize) -> (String, bool) {
    if max_lines == 0 {
        return (
            "[wave-vcs patch truncated: line budget is 0]\n".to_string(),
            true,
        );
    }
    let mut lines = text.lines().collect::<Vec<_>>();
    if lines.len() <= max_lines {
        return (text, false);
    }
    lines.truncate(max_lines);
    let mut out = lines.join("\n");
    out.push('\n');
    out.push_str(&format!(
        "[wave-vcs patch truncated after {max_lines} lines]\n"
    ));
    (out, true)
}

fn path_matches(changed: &str, requested: &str) -> bool {
    changed == requested || changed.starts_with(&format!("{requested}/"))
}

async fn changed_paths_for_commit(pool: &SqlitePool, record: &CommitRecord) -> Result<Vec<String>> {
    let Some(tree) = load_tree_object_pool(pool, &record.tree_hash).await? else {
        return Ok(Vec::new());
    };
    let entries = if let Some(parent_hash) = record.parent_hash.as_deref() {
        let Some(parent) = tree_at(pool, parent_hash).await? else {
            return Ok(Vec::new());
        };
        diff_manifests(&parent, &tree, None)
            .into_iter()
            .map(|entry| entry.path)
            .collect()
    } else {
        tree.entries.keys().cloned().collect()
    };
    Ok(entries)
}

fn is_internal_observation_diff_path(path: &str, spec_card_id: Option<&CardId>) -> bool {
    if path.starts_with("cards/") && path.ends_with("/runtime.json") {
        return true;
    }
    let Some(spec_card_id) = spec_card_id else {
        return false;
    };
    let spec_card_id = spec_card_id.as_str();
    // Legacy spellings appear once per wave in the post-rename healing commit.
    path == format!("cards/{spec_card_id}/.payload.json")
        || path == format!("cards/{spec_card_id}/payload.json")
}

fn short_hash(hash: &str) -> &str {
    hash.get(..8).unwrap_or(hash)
}

fn markdown_code_fence_for(text: &str) -> String {
    let mut longest = 0;
    let mut current = 0;
    for ch in text.chars() {
        if ch == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    "`".repeat(3.max(longest + 1))
}
