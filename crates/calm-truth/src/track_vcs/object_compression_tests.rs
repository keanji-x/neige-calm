use super::object_codec::{MAX_DECODED_BYTES, StoredObject, encode};
use super::*;
use crate::db::prelude::RepoSyncDomainRaw;
use crate::db::sqlite::{SqlxRepo, begin_immediate_tx};
use crate::model::{NewArea, NewTrack, RequestTheme};
use calm_types::organization::LEGACY_OWNER_USER_ID;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::Notify;

tokio::task_local! {
    static PREPARED: Arc<(Notify, Notify)>;
}

pub(super) async fn after_prepare() {
    if let Ok(pause) = PREPARED.try_with(Arc::clone) {
        pause.0.notify_one();
        pause.1.notified().await;
    }
}

fn encoded_blob(bytes: &[u8]) -> StoredObject {
    let encoded = encode(bytes).unwrap();
    assert_eq!(encoded.encoding, "zlib-v1");
    StoredObject {
        hash: super::store::hash_bytes("blob", bytes),
        kind: "blob".into(),
        bytes: encoded.bytes,
        encoding: encoded.encoding.into(),
        decoded_size: encoded.decoded_size,
    }
}

#[test]
fn object_codec_rejects_corrupt_truncated_trailing_and_wrong_identity() {
    let raw = b"report\n".repeat(4096);
    assert_eq!(encoded_blob(&raw).blob().unwrap(), raw);
    for removed in [1, 4, 8] {
        let mut object = encoded_blob(&raw);
        object.bytes.truncate(object.bytes.len() - removed);
        assert!(
            object.blob().is_err(),
            "accepted stream truncated by {removed}"
        );
    }
    let mut trailing = encoded_blob(&raw);
    trailing.bytes.push(0);
    assert!(trailing.blob().is_err());
    let mut corrupt = encoded_blob(&raw);
    let i = corrupt.bytes.len() / 2;
    corrupt.bytes[i] ^= 1;
    assert!(corrupt.blob().is_err());
    let mut wrong_hash = encoded_blob(&raw);
    wrong_hash.hash = "0".repeat(64);
    assert!(wrong_hash.blob().is_err());
}

#[test]
fn object_codec_enforces_decoded_limit_and_exact_size() {
    let raw = b"x".repeat(16384);
    for size in [-1, 0, 1, 16383, 16385, MAX_DECODED_BYTES as i64 + 1] {
        let mut object = encoded_blob(&raw);
        object.decoded_size = Some(size);
        assert!(object.blob().is_err(), "accepted decoded size {size}");
    }
    let mut unknown = encoded_blob(&raw);
    unknown.encoding = "future".into();
    assert!(unknown.blob().is_err());
}

#[test]
fn object_codec_preserves_arbitrary_legacy_raw_and_small_objects() {
    let raw = b"\x78\x9cnot actually a zlib stream\0".to_vec();
    let encoded = encode(&raw).unwrap();
    assert_eq!(encoded.encoding, "raw");
    let object = StoredObject {
        hash: "legacy".into(),
        kind: "blob".into(),
        bytes: raw.clone(),
        encoding: "raw".into(),
        decoded_size: None,
    };
    assert_eq!(object.blob().unwrap(), raw);
}

#[tokio::test]
async fn object_storage_large_write_defers_cpu_to_outside_writer_conversion() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let raw = b"large historical document\n".repeat(32768);
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let hash = put_blob(&mut tx, "blob", &raw).await.unwrap();
    tx.commit().await.unwrap();
    let encoding: String =
        sqlx::query_scalar("SELECT encoding FROM track_vcs_objects WHERE hash=?")
            .bind(&hash)
            .fetch_one(repo.db().read_pool())
            .await
            .unwrap();
    assert_eq!(encoding, "raw");
    assert_eq!(
        object_compression::convert_page(repo.db(), "")
            .await
            .unwrap()
            .converted,
        1
    );
    assert_eq!(
        super::store::load_blob_bytes_pool(repo.db().read_pool(), &hash)
            .await
            .unwrap()
            .unwrap(),
        raw
    );
}

#[tokio::test]
async fn object_storage_compresses_repetitive_payload_without_changing_identity() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let bytes = b"historical report content\n".repeat(4096);
    let expected = blake3::hash(&[b"calm-track-vcs-v1\0blob\0".as_slice(), &bytes].concat())
        .to_hex()
        .to_string();
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let hash = put_blob(&mut tx, "blob", &bytes).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(hash, expected);
    let stored: i64 =
        sqlx::query_scalar("SELECT length(bytes) FROM track_vcs_objects WHERE hash=?")
            .bind(&hash)
            .fetch_one(repo.db().read_pool())
            .await
            .unwrap();
    assert!(stored < bytes.len() as i64 / 2, "stored {stored} bytes");
    assert_eq!(
        super::store::load_blob_bytes_pool(repo.db().read_pool(), &hash)
            .await
            .unwrap()
            .unwrap(),
        bytes
    );
}

#[tokio::test]
async fn object_conversion_preview_resume_and_replay_preserve_bytes_and_identity() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let mut hashes = Vec::new();
    for i in 0..40 {
        let raw = format!("historical output {i}\n").repeat(2048).into_bytes();
        let hash = super::store::hash_bytes("blob", &raw);
        sqlx::query(
            "INSERT INTO track_vcs_objects(hash,kind,bytes,created_at) VALUES(?,'blob',?,0)",
        )
        .bind(&hash)
        .bind(&raw)
        .execute(repo.db().writer_pool())
        .await
        .unwrap();
        hashes.push((hash, raw));
    }
    object_compression::require_schema(repo.db().read_pool())
        .await
        .unwrap();
    let revision: i64 = sqlx::query_scalar("SELECT revision FROM track_vcs_gc_state")
        .fetch_one(repo.db().read_pool())
        .await
        .unwrap();
    let preview = object_compression::preview_page(repo.db().read_pool(), "")
        .await
        .unwrap();
    assert_eq!(preview.candidates, 32);
    assert_eq!(preview.converted, 0);
    assert!(!preview.complete);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT revision FROM track_vcs_gc_state")
            .fetch_one(repo.db().read_pool())
            .await
            .unwrap(),
        revision
    );
    let first = object_compression::convert_page(repo.db(), "")
        .await
        .unwrap();
    assert_eq!(first.converted, 32);
    assert_eq!(first.payload_bytes_saved, preview.payload_bytes_saved);
    assert_eq!(first.after, preview.after);
    let replay = object_compression::convert_page(repo.db(), "")
        .await
        .unwrap();
    assert_eq!(replay.converted, 0);
    let second = object_compression::convert_page(repo.db(), &first.after)
        .await
        .unwrap();
    assert_eq!(second.converted, 8);
    assert!(second.complete);
    for (hash, raw) in hashes {
        assert_eq!(
            super::store::load_blob_bytes_pool(repo.db().read_pool(), &hash)
                .await
                .unwrap()
                .unwrap(),
            raw
        );
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM track_vcs_objects")
            .fetch_one(repo.db().read_pool())
            .await
            .unwrap(),
        40
    );
}

#[tokio::test]
async fn object_storage_history_cat_diff_log_and_gc_preserve_mixed_roots() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = repo
        .area_create(
            LEGACY_OWNER_USER_ID,
            NewArea {
                name: "compression".into(),
                color: "#111111".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            area_id: area.id,
            title: "history".into(),
            sort: None,
            cwd: "/compression-test".into(),
            template_id: None,
            template_input: None,
            plugin_scope: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let before = "old report\n".repeat(4096);
    let after = "new report\n".repeat(4096);
    let mut parent = None;
    let mut commits = Vec::new();
    for (n, content) in [&before, &after].into_iter().enumerate() {
        let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
        let blob = put_blob(&mut tx, "blob", content.as_bytes()).await.unwrap();
        if n == 0 {
            sqlx::query("UPDATE track_vcs_objects SET bytes=?,encoding='raw',decoded_size=NULL WHERE hash=?")
                .bind(content.as_bytes()).bind(&blob).execute(&mut **tx).await.unwrap();
        }
        let entries = (0..128)
            .map(|i| {
                (
                    format!("documents/{i}.md"),
                    ManifestEntry {
                        blob_hash: blob.clone(),
                        byte_len: content.len() as u64,
                        content_type: "text/markdown".into(),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let tree = super::store::store_tree(&mut tx, 1, entries, n as i64)
            .await
            .unwrap();
        let commit = commit_tree(
            &mut tx,
            &track.id,
            parent.as_deref(),
            &tree,
            None,
            "report revision",
            n as i64,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        parent = Some(commit.clone());
        commits.push(commit);
    }
    assert_eq!(
        cat_at(repo.db().read_pool(), &commits[0], "documents/0.md")
            .await
            .unwrap()
            .content,
        before
    );
    assert_eq!(
        cat_at(repo.db().read_pool(), &commits[1], "documents/0.md")
            .await
            .unwrap()
            .content,
        after
    );
    let delta = diff_with_patches(
        repo.db().read_pool(),
        &commits[0],
        &commits[1],
        Some("documents/0.md"),
        20,
    )
    .await
    .unwrap();
    assert_eq!(delta.len(), 1);
    assert_eq!(delta[0].status, DiffStatus::Modified);
    assert!(delta[0].patch.as_ref().unwrap().contains("old report"));
    let history = log(repo.db().read_pool(), &track.id, None, None, 50, false)
        .await
        .unwrap();
    assert_eq!(history.commits.len(), 2);
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let orphan = put_blob(&mut tx, "blob", &b"unreferenced diagnostic\n".repeat(4096))
        .await
        .unwrap();
    sqlx::query("UPDATE track_vcs_objects SET created_at=0")
        .execute(&mut **tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(sweep_unreferenced_objects_once(repo.db()).await.unwrap(), 1);
    assert!(
        super::store::load_blob_bytes_pool(repo.db().read_pool(), &orphan)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        cat_at(repo.db().read_pool(), &commits[0], "documents/0.md")
            .await
            .unwrap()
            .content,
        before
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM track_vcs_objects WHERE encoding='zlib-v1'"
        )
        .fetch_one(repo.db().read_pool())
        .await
        .unwrap(),
        3
    );
}

#[tokio::test]
async fn object_conversion_rechecks_after_concurrent_sweep_or_conversion() {
    for sweep in [true, false] {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let raw = b"historical output\n".repeat(4096);
        let hash = super::store::hash_bytes("blob", &raw);
        sqlx::query(
            "INSERT INTO track_vcs_objects(hash,kind,bytes,created_at) VALUES(?,'blob',?,0)",
        )
        .bind(&hash)
        .bind(&raw)
        .execute(repo.db().writer_pool())
        .await
        .unwrap();
        let pause = Arc::new((Notify::new(), Notify::new()));
        let db = repo.db().clone();
        let task = tokio::spawn(PREPARED.scope(pause.clone(), async move {
            object_compression::convert_page(&db, "").await
        }));
        tokio::time::timeout(std::time::Duration::from_secs(5), pause.0.notified())
            .await
            .unwrap();
        if sweep {
            assert_eq!(sweep_unreferenced_objects_once(repo.db()).await.unwrap(), 1);
        } else {
            assert_eq!(
                object_compression::convert_page(repo.db(), "")
                    .await
                    .unwrap()
                    .converted,
                1
            );
        }
        pause.1.notify_one();
        let report = task.await.unwrap().unwrap();
        assert_eq!(report.converted, 0);
        assert_eq!(report.changed_since_read, 1);
        assert_eq!(report.payload_bytes_saved, 0);
        let stored = super::store::load_blob_bytes_pool(repo.db().read_pool(), &hash)
            .await
            .unwrap();
        if sweep {
            assert!(stored.is_none());
        } else {
            assert_eq!(stored.unwrap(), raw);
        }
    }
}
