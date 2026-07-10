use crate::error::{CalmError, Result};
use crate::ids::WaveId;
use crate::model::now_ms;
use serde::Serialize;
use serde_json::Value;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, Sqlite, SqlitePool, Transaction};
use std::collections::BTreeMap;

use super::types::BlobContent;
use super::{CommitHash, CommitRecord, ManifestEntry, ObjectHash, TreeManifest, TreeSnapshot};

pub async fn put_blob(
    tx: &mut Transaction<'_, Sqlite>,
    kind: &str,
    bytes: &[u8],
) -> Result<ObjectHash> {
    put_object_at_tx(tx, kind, bytes, now_ms()).await
}

pub async fn head(pool: &SqlitePool, wave_id: &WaveId) -> Result<Option<CommitHash>> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT head_hash FROM wave_vcs_refs WHERE wave_id = ?1")
            .bind(wave_id.as_str())
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(hash,)| hash))
}

pub async fn tree_at(pool: &SqlitePool, commit_hash: &str) -> Result<Option<TreeManifest>> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT tree_hash FROM wave_vcs_commits WHERE hash = ?1")
            .bind(commit_hash)
            .fetch_optional(pool)
            .await?;
    let Some((tree_hash,)) = row else {
        return Ok(None);
    };
    load_tree_object_pool(pool, &tree_hash).await
}

#[derive(Clone, Copy)]
pub(super) struct CommitTreeMeta<'a> {
    pub(super) parent_hash: Option<&'a str>,
    pub(super) author: Option<&'a str>,
    pub(super) event_id: Option<i64>,
    pub(super) message: &'a str,
    pub(super) manifest_schema_version: i64,
    pub(super) created_at: i64,
}

pub(super) fn commit_hash_for_tree(
    wave_id: &WaveId,
    tree_hash: &str,
    lifecycle: &str,
    meta: &CommitTreeMeta<'_>,
) -> Result<CommitHash> {
    let mut commit = BTreeMap::<String, Value>::new();
    commit.insert("created_at".into(), Value::from(meta.created_at));
    commit.insert(
        "event_id".into(),
        meta.event_id.map(Value::from).unwrap_or(Value::Null),
    );
    commit.insert("lifecycle".into(), Value::String(lifecycle.to_string()));
    commit.insert(
        "manifest_schema_version".into(),
        Value::from(meta.manifest_schema_version),
    );
    commit.insert("message".into(), Value::String(meta.message.to_string()));
    commit.insert(
        "parent_hash".into(),
        meta.parent_hash
            .map(|hash| Value::String(hash.to_string()))
            .unwrap_or(Value::Null),
    );
    commit.insert("tree_hash".into(), Value::String(tree_hash.to_string()));
    commit.insert("wave_id".into(), Value::String(wave_id.to_string()));
    let commit_bytes = canonical_json_bytes(&commit)?;
    Ok(hash_bytes("commit", &commit_bytes))
}

pub(super) async fn commit_tree_at_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    tree: &TreeSnapshot,
    meta: CommitTreeMeta<'_>,
) -> Result<CommitHash> {
    let lifecycle = wave_lifecycle_tx(tx, wave_id).await?;
    let hash = commit_hash_for_tree(wave_id, &tree.tree_hash, &lifecycle, &meta)?;

    sqlx::query(
        r#"INSERT OR IGNORE INTO wave_vcs_commits (
               hash, wave_id, parent_hash, tree_hash, manifest_schema_version,
               author, message, lifecycle, event_id, created_at
           )
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"#,
    )
    .bind(&hash)
    .bind(wave_id.as_str())
    .bind(meta.parent_hash)
    .bind(&tree.tree_hash)
    .bind(meta.manifest_schema_version)
    .bind(meta.author)
    .bind(meta.message)
    .bind(&lifecycle)
    .bind(meta.event_id)
    .bind(meta.created_at)
    .execute(&mut **tx)
    .await?;

    sqlx::query(
        r#"INSERT INTO wave_vcs_refs (wave_id, head_hash, updated_event_id)
           VALUES (?1, ?2, ?3)
           ON CONFLICT(wave_id) DO UPDATE SET
             head_hash = excluded.head_hash,
             updated_event_id = excluded.updated_event_id"#,
    )
    .bind(wave_id.as_str())
    .bind(&hash)
    .bind(meta.event_id)
    .execute(&mut **tx)
    .await?;

    Ok(hash)
}

pub(super) async fn store_tree(
    tx: &mut Transaction<'_, Sqlite>,
    schema_version: i64,
    entries: BTreeMap<String, ManifestEntry>,
    created_at: i64,
) -> Result<TreeSnapshot> {
    let manifest = TreeManifest {
        schema_version,
        entries,
    };
    let tree_hash = hash_tree_manifest(&manifest);
    let bytes = canonical_json_bytes(&manifest)?;
    sqlx::query(
        r#"INSERT OR IGNORE INTO wave_vcs_objects (hash, kind, bytes, created_at)
           VALUES (?1, 'tree', ?2, ?3)"#,
    )
    .bind(&tree_hash)
    .bind(&bytes)
    .bind(created_at)
    .execute(&mut **tx)
    .await?;
    Ok(TreeSnapshot {
        tree_hash,
        manifest,
    })
}

pub(super) async fn put_rendered_entry(
    tx: &mut Transaction<'_, Sqlite>,
    entries: &mut BTreeMap<String, ManifestEntry>,
    path: impl Into<String>,
    content: BlobContent,
    created_at: i64,
) -> Result<()> {
    let hash = put_object_at_tx(tx, "blob", &content.bytes, created_at).await?;
    entries.insert(
        path.into(),
        ManifestEntry {
            blob_hash: hash,
            byte_len: content.bytes.len() as u64,
            content_type: content.content_type,
        },
    );
    Ok(())
}

pub(super) async fn load_blob_bytes_tx(
    tx: &mut Transaction<'_, Sqlite>,
    hash: &str,
) -> Result<Option<Vec<u8>>> {
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT bytes FROM wave_vcs_objects WHERE hash = ?1 AND kind = 'blob'")
            .bind(hash)
            .fetch_optional(&mut **tx)
            .await?;
    Ok(row.map(|(bytes,)| bytes))
}

pub(super) async fn load_blob_bytes_pool(pool: &SqlitePool, hash: &str) -> Result<Option<Vec<u8>>> {
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT bytes FROM wave_vcs_objects WHERE hash = ?1 AND kind = 'blob'")
            .bind(hash)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(bytes,)| bytes))
}

async fn put_object_at_tx(
    tx: &mut Transaction<'_, Sqlite>,
    kind: &str,
    bytes: &[u8],
    created_at: i64,
) -> Result<ObjectHash> {
    let hash = hash_bytes(kind, bytes);
    sqlx::query(
        r#"INSERT OR IGNORE INTO wave_vcs_objects (hash, kind, bytes, created_at)
           VALUES (?1, ?2, ?3, ?4)"#,
    )
    .bind(&hash)
    .bind(kind)
    .bind(bytes)
    .bind(created_at)
    .execute(&mut **tx)
    .await?;
    Ok(hash)
}

pub(super) async fn head_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
) -> Result<Option<CommitHash>> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT head_hash FROM wave_vcs_refs WHERE wave_id = ?1")
            .bind(wave_id.as_str())
            .fetch_optional(&mut **tx)
            .await?;
    Ok(row.map(|(hash,)| hash))
}

pub(super) async fn tree_at_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    commit_hash: &str,
) -> Result<Option<TreeManifest>> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT tree_hash FROM wave_vcs_commits WHERE hash = ?1")
            .bind(commit_hash)
            .fetch_optional(&mut **tx)
            .await?;
    let Some((tree_hash,)) = row else {
        return Ok(None);
    };
    load_tree_object_tx(tx, &tree_hash).await
}

async fn load_tree_object_tx(
    tx: &mut Transaction<'_, Sqlite>,
    tree_hash: &str,
) -> Result<Option<TreeManifest>> {
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT bytes FROM wave_vcs_objects WHERE hash = ?1 AND kind = 'tree'")
            .bind(tree_hash)
            .fetch_optional(&mut **tx)
            .await?;
    row.map(|(bytes,)| serde_json::from_slice(&bytes).map_err(Into::into))
        .transpose()
}

pub(super) async fn load_tree_object_pool(
    pool: &SqlitePool,
    tree_hash: &str,
) -> Result<Option<TreeManifest>> {
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT bytes FROM wave_vcs_objects WHERE hash = ?1 AND kind = 'tree'")
            .bind(tree_hash)
            .fetch_optional(pool)
            .await?;
    row.map(|(bytes,)| serde_json::from_slice(&bytes).map_err(Into::into))
        .transpose()
}

pub(super) async fn load_commit_record_pool(
    pool: &SqlitePool,
    commit_hash: &str,
) -> Result<Option<CommitRecord>> {
    let row = sqlx::query(
        r#"SELECT hash, wave_id, parent_hash, tree_hash, manifest_schema_version,
                  lifecycle, event_id, created_at, message, author
           FROM wave_vcs_commits
           WHERE hash = ?1"#,
    )
    .bind(commit_hash)
    .fetch_optional(pool)
    .await?;
    row.map(commit_record_from_row).transpose()
}

pub(super) async fn load_commit_record_for_wave_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    commit_hash: &str,
) -> Result<Option<CommitRecord>> {
    let row = sqlx::query(
        r#"SELECT hash, wave_id, parent_hash, tree_hash, manifest_schema_version,
                  lifecycle, event_id, created_at, message, author
           FROM wave_vcs_commits
           WHERE hash = ?1
             AND wave_id = ?2"#,
    )
    .bind(commit_hash)
    .bind(wave_id.as_str())
    .fetch_optional(&mut **tx)
    .await?;
    row.map(commit_record_from_row).transpose()
}

pub(super) async fn commit_records_for_wave_pool(
    pool: &SqlitePool,
    wave_id: &WaveId,
    limit: usize,
) -> Result<Vec<CommitRecord>> {
    let rows = sqlx::query(
        r#"SELECT hash, wave_id, parent_hash, tree_hash, manifest_schema_version,
                  lifecycle, event_id, created_at, message, author
           FROM wave_vcs_commits
           WHERE wave_id = ?1
           ORDER BY created_at DESC, COALESCE(event_id, -1) DESC, hash DESC
           LIMIT ?2"#,
    )
    .bind(wave_id.as_str())
    .bind(limit as i64)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(commit_record_from_row).collect()
}

fn commit_record_from_row(row: SqliteRow) -> Result<CommitRecord> {
    Ok(CommitRecord {
        hash: row.try_get("hash")?,
        wave_id: WaveId::from(row.try_get::<String, _>("wave_id")?),
        parent_hash: row.try_get("parent_hash")?,
        tree_hash: row.try_get("tree_hash")?,
        manifest_schema_version: row.try_get("manifest_schema_version")?,
        lifecycle: row.try_get("lifecycle")?,
        event_id: row.try_get("event_id")?,
        created_at: row.try_get("created_at")?,
        message: row.try_get("message")?,
        author: row.try_get("author")?,
    })
}

async fn wave_lifecycle_tx(tx: &mut Transaction<'_, Sqlite>, wave_id: &WaveId) -> Result<String> {
    let row: Option<(String,)> = sqlx::query_as("SELECT lifecycle FROM waves WHERE id = ?1")
        .bind(wave_id.as_str())
        .fetch_optional(&mut **tx)
        .await?;
    row.map(|(lifecycle,)| lifecycle)
        .ok_or_else(|| CalmError::NotFound(format!("wave {}", wave_id.as_str())))
}

pub fn canonical_json_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let value = serde_json::to_value(value)?;
    let mut out = Vec::new();
    write_canonical_json(&mut out, &value)?;
    Ok(out)
}

fn write_canonical_json(out: &mut Vec<u8>, value: &Value) -> Result<()> {
    match value {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(v) => out.extend_from_slice(if *v { b"true" } else { b"false" }),
        Value::Number(number) => out.extend_from_slice(number.to_string().as_bytes()),
        Value::String(s) => serde_json::to_writer(out, s)?,
        Value::Array(values) => {
            out.push(b'[');
            for (idx, value) in values.iter().enumerate() {
                if idx > 0 {
                    out.push(b',');
                }
                write_canonical_json(out, value)?;
            }
            out.push(b']');
        }
        Value::Object(map) => {
            out.push(b'{');
            let mut first = true;
            for (key, value) in map.iter().collect::<BTreeMap<_, _>>() {
                if !first {
                    out.push(b',');
                }
                first = false;
                serde_json::to_writer(&mut *out, key)?;
                out.push(b':');
                write_canonical_json(out, value)?;
            }
            out.push(b'}');
        }
    }
    Ok(())
}

fn hash_tree_manifest(manifest: &TreeManifest) -> ObjectHash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"calm-wave-vcs-v1\0tree\0");
    hasher.update(manifest.schema_version.to_string().as_bytes());
    hasher.update(b"\0");
    for (path, entry) in &manifest.entries {
        hasher.update(path.as_bytes());
        hasher.update(b"\0");
        hasher.update(entry.blob_hash.as_bytes());
        hasher.update(b"\0");
        hasher.update(entry.byte_len.to_string().as_bytes());
        hasher.update(b"\0");
        hasher.update(entry.content_type.as_bytes());
        hasher.update(b"\0");
    }
    hasher.finalize().to_hex().to_string()
}

fn hash_bytes(kind: &str, bytes: &[u8]) -> ObjectHash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"calm-wave-vcs-v1\0");
    hasher.update(kind.as_bytes());
    hasher.update(b"\0");
    hasher.update(bytes);
    hasher.finalize().to_hex().to_string()
}

pub(super) fn normalize_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed == "/" {
        return String::new();
    }
    trimmed
        .trim_start_matches('/')
        .trim_end_matches('/')
        .to_string()
}
