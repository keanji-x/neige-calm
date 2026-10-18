//! Task-local observability at the real sweep phases; no alternate GC implementation.
use crate::db::prelude::RepoSyncDomainRaw;
use crate::db::sqlite::{SqlxRepo, begin_immediate_tx};
use crate::ids::TrackId;
use crate::model::{NewArea, NewTrack, RequestTheme, now_ms};
use crate::track_vcs::{self, ManifestEntry, TreeSnapshot, commit_tree, put_blob};
use calm_types::organization::LEGACY_OWNER_USER_ID;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::Notify;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Phase {
    Parse,
    AfterMetadata,
    BeforeDelete,
}
pub(super) struct Pause {
    entered: Notify,
    release: Notify,
    steps: Vec<Phase>,
    next: AtomicUsize,
    metadata_queries: AtomicUsize,
}
tokio::task_local! { static SWEEP_PAUSE: Arc<Pause>; }
tokio::task_local! { pub(super) static DELETE_VALIDATIONS: Arc<AtomicUsize>; }
tokio::task_local! { pub(super) static METADATA_QUERIES: Arc<AtomicUsize>; }
pub(super) fn record_metadata_query() {
    let _ = METADATA_QUERIES.try_with(|counter| counter.fetch_add(1, Ordering::SeqCst));
    let _ = SWEEP_PAUSE.try_with(|pause| pause.metadata_queries.fetch_add(1, Ordering::SeqCst));
}
pub(super) async fn at_phase(phase: Phase) {
    if phase == Phase::BeforeDelete {
        let _ = DELETE_VALIDATIONS.try_with(|counter| counter.fetch_add(1, Ordering::SeqCst));
    }
    let Ok(pause) = SWEEP_PAUSE.try_with(Arc::clone) else {
        return;
    };
    let step = pause.next.load(Ordering::SeqCst);
    if pause.steps.get(step) == Some(&phase) {
        pause.next.fetch_add(1, Ordering::SeqCst);
        pause.entered.notify_one();
        pause.release.notified().await;
    }
}
pub(super) async fn pause_before_manifest_parse() {
    at_phase(Phase::Parse).await;
}
impl Pause {
    pub(super) fn metadata_queries(&self) -> usize {
        self.metadata_queries.load(Ordering::SeqCst)
    }
    pub(super) async fn entered(&self) {
        tokio::time::timeout(Duration::from_secs(10), self.entered.notified())
            .await
            .expect("public sweep reached the requested production phase");
    }
    pub(super) fn release(&self) {
        self.release.notify_one();
    }
}
pub(super) fn start(
    pool: crate::db::sqlite::SqliteDb,
    steps: Vec<Phase>,
) -> (
    Arc<Pause>,
    tokio::task::JoinHandle<crate::error::Result<u64>>,
) {
    let pause = Arc::new(Pause {
        entered: Notify::new(),
        release: Notify::new(),
        steps,
        next: AtomicUsize::new(0),
        metadata_queries: AtomicUsize::new(0),
    });
    let task = tokio::spawn(SWEEP_PAUSE.scope(pause.clone(), async move {
        track_vcs::sweep_unreferenced_objects_once(&pool).await
    }));
    (pause, task)
}
pub(super) async fn finish(
    task: tokio::task::JoinHandle<crate::error::Result<u64>>,
) -> crate::error::Result<u64> {
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap()
}

pub(super) struct Fixture {
    _directory: tempfile::TempDir,
    pub(super) repo: SqlxRepo,
    pub(super) track: TrackId,
    pub(super) tree: TreeSnapshot,
    pub(super) commit: String,
}
impl Fixture {
    pub(super) async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("gc-writer.db");
        let repo = SqlxRepo::open(&format!("sqlite://{}?mode=rwc", path.display()))
            .await
            .unwrap();
        let area = repo
            .area_create(
                LEGACY_OWNER_USER_ID,
                NewArea {
                    name: "GC regression".into(),
                    color: "#111111".into(),
                    sort: None,
                },
            )
            .await
            .unwrap();
        let track = repo
            .track_create(NewTrack {
                template_input: None,
                area_id: area.id,
                title: "reachable tree".into(),
                sort: None,
                cwd: "/synthetic-gc-writer".into(),
                template_id: None,
                plugin_scope: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
        let blob = put_blob(&mut tx, "blob", b"retained document")
            .await
            .unwrap();
        let entries = BTreeMap::from([(
            "documents/report.md".into(),
            ManifestEntry {
                blob_hash: blob,
                byte_len: 17,
                content_type: "text/markdown".into(),
            },
        )]);
        let tree = crate::track_vcs::store::store_tree(&mut tx, 1, entries, now_ms())
            .await
            .unwrap();
        let commit = commit_tree(&mut tx, &track.id, None, &tree, None, "GC fixture", 1)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE unrelated_gc_writer_probe (value INTEGER NOT NULL)")
            .execute(&mut **tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        Self {
            _directory: directory,
            repo,
            track: track.id,
            tree,
            commit,
        }
    }
    pub(super) async fn exhaust_revision(&self) {
        // Seed the otherwise valid representable final integer state without 2^63 writes;
        // restore the exact real migration-created guard SQL before exercising mutation.
        let mut tx = begin_immediate_tx(self.repo.db()).await.unwrap();
        let guard: String = sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE type='trigger' AND name='track_vcs_gc_state_monotonic'")
        .fetch_one(&mut **tx).await.unwrap();
        sqlx::query("DROP TRIGGER track_vcs_gc_state_monotonic")
            .execute(&mut **tx)
            .await
            .unwrap();
        sqlx::query("UPDATE track_vcs_gc_state SET revision=9223372036854775807")
            .execute(&mut **tx)
            .await
            .unwrap();
        sqlx::query(&guard).execute(&mut **tx).await.unwrap();
        tx.commit().await.unwrap();
    }

    pub(super) async fn remove_required_revision(&self) {
        // Isolated corruption fixture: supported DML cannot remove this required state.
        // Schema-sensitive steps share one physical connection and commit atomically.
        let mut tx = begin_immediate_tx(self.repo.db()).await.unwrap();
        sqlx::query("DROP TRIGGER track_vcs_gc_state_no_delete")
            .execute(&mut **tx)
            .await
            .unwrap();
        sqlx::query("DELETE FROM track_vcs_gc_state")
            .execute(&mut **tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    pub(super) async fn tree(&self, label: &str, count: usize) -> TreeSnapshot {
        let mut tx = begin_immediate_tx(self.repo.db()).await.unwrap();
        let blob = put_blob(&mut tx, "blob", label.as_bytes()).await.unwrap();
        let entries = (0..count)
            .map(|i| {
                (
                    format!("documents/{i:032x}.md"),
                    ManifestEntry {
                        blob_hash: blob.clone(),
                        byte_len: label.len() as u64,
                        content_type: "text/markdown".into(),
                    },
                )
            })
            .collect();
        let tree = crate::track_vcs::store::store_tree(&mut tx, 1, entries, now_ms())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        tree
    }
    pub(super) async fn publish(&self, tree: &TreeSnapshot, parent: Option<&str>) -> String {
        let mut tx = begin_immediate_tx(self.repo.db()).await.unwrap();
        let commit = commit_tree(&mut tx, &self.track, parent, tree, None, "published", 1)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        commit
    }
    pub(super) async fn orphan(&self, label: &str) -> String {
        let mut tx = begin_immediate_tx(self.repo.db()).await.unwrap();
        let blob = put_blob(&mut tx, "blob", label.as_bytes()).await.unwrap();
        sqlx::query("UPDATE track_vcs_objects SET created_at=?1 WHERE hash=?2")
            .bind(now_ms() - 7_200_000)
            .bind(&blob)
            .execute(&mut **tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        blob
    }
    pub(super) async fn age(&self, tree: &TreeSnapshot) {
        let mut tx = begin_immediate_tx(self.repo.db()).await.unwrap();
        for hash in std::iter::once(&tree.tree_hash)
            .chain(tree.manifest.entries.values().map(|entry| &entry.blob_hash))
        {
            sqlx::query("UPDATE track_vcs_objects SET created_at=?1 WHERE hash=?2")
                .bind(now_ms() - 7_200_000)
                .bind(hash)
                .execute(&mut **tx)
                .await
                .unwrap();
        }
        tx.commit().await.unwrap();
    }
    pub(super) async fn exists(&self, hash: &str) -> bool {
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM track_vcs_objects WHERE hash=?1)")
            .bind(hash)
            .fetch_one(self.repo.pool())
            .await
            .unwrap()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn object_sweep_manifest_parsing_does_not_hold_database_writer() {
    let f = Fixture::new().await;
    let (pause, sweep) = start(f.repo.db().clone(), vec![Phase::Parse]);
    pause.entered().await;
    let pool = f.repo.db().clone();
    let mut writer = tokio::spawn(async move {
        let mut tx = begin_immediate_tx(&pool).await?;
        sqlx::query("INSERT INTO unrelated_gc_writer_probe(value) VALUES (1)")
            .execute(&mut **tx)
            .await?;
        tx.commit().await?;
        Ok::<_, crate::error::CalmError>(())
    });
    let committed_before_parse_release =
        tokio::time::timeout(Duration::from_secs(1), &mut writer).await;
    // Drain both real tasks before reporting the assertion, including an expected red run.
    pause.release();
    finish(sweep).await.unwrap();
    if committed_before_parse_release.is_err() {
        tokio::time::timeout(Duration::from_secs(10), writer)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    assert!(
        matches!(committed_before_parse_release, Ok(Ok(Ok(())))),
        "unrelated writer waited for manifest parsing: {committed_before_parse_release:?}"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM unrelated_gc_writer_probe")
            .fetch_one(f.repo.pool())
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        track_vcs::cat_at(f.repo.pool(), &f.commit, "documents/report.md")
            .await
            .unwrap()
            .content,
        "retained document"
    );
}
