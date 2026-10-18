use super::gc::{
    DEFAULT_TRACK_HISTORY_PRUNE_KEEP, TRACK_HISTORY_PRUNE_INTERVAL,
    TRACK_HISTORY_PRUNE_INTERVAL_SECS_ENV, TRACK_HISTORY_PRUNE_KEEP_ENV,
    track_history_pruner_config_from_env,
};
use super::store::{
    COMMIT_PREFIX_QUERY, CommitTreeMeta, commit_hash_for_tree, commit_records_for_track_pool,
};
use super::*;
use crate::db::prelude::*;
use crate::db::sqlite::{SqlxRepo, begin_immediate_tx};
use crate::event::{Event, ForgeMergeSubject};
use crate::ids::{ActorId, CardId, TrackId};
use crate::model::{NewArea, NewTrack, RequestTheme};
use sqlx::Row;
use std::time::Duration;

#[test]
fn commit_hash_ignores_author_metadata() {
    let track_id = TrackId::from("track-1");
    let base = CommitTreeMeta {
        parent_hash: Some("parent-1"),
        author: Some("user"),
        event_id: Some(7),
        message: "track.updated",
        manifest_schema_version: MANIFEST_SCHEMA_VERSION,
        created_at: 1234,
    };
    let other_author = CommitTreeMeta {
        author: Some("kernel"),
        ..base
    };

    assert_eq!(
        commit_hash_for_tree(&track_id, "tree-1", &base).unwrap(),
        commit_hash_for_tree(&track_id, "tree-1", &other_author).unwrap()
    );
}

#[tokio::test]
async fn commit_prefix_query_uses_track_hash_index_without_temporary_sorting() {
    let repo = SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open sqlite repo");
    let explain = format!("EXPLAIN QUERY PLAN {COMMIT_PREFIX_QUERY}");
    let details = sqlx::query(&explain)
        .bind("track-1")
        .bind("deadbeef")
        .bind("deadbeefg")
        .fetch_all(repo.pool())
        .await
        .expect("explain commit prefix query")
        .into_iter()
        .map(|row| row.get::<String, _>("detail"))
        .collect::<Vec<_>>();
    let plan = details.join("\n");

    assert!(
        plan.contains("idx_track_vcs_commits_track_hash"),
        "prefix lookup must use the track/hash index: {plan}"
    );
    assert!(
        !plan.contains("TEMP B-TREE"),
        "prefix lookup must preserve index order: {plan}"
    );
}

#[tokio::test]
async fn commit_log_keyset_pages_ignore_newer_concurrent_commits() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:")
        .await
        .expect("open sqlite pool");
    sqlx::query(
        r#"CREATE TABLE track_vcs_commits (
               hash TEXT PRIMARY KEY,
               track_id TEXT NOT NULL,
               parent_hash TEXT,
               tree_hash TEXT NOT NULL,
               manifest_schema_version INTEGER NOT NULL,
               author TEXT,
               message TEXT,
               event_id INTEGER,
               created_at INTEGER NOT NULL
           )"#,
    )
    .execute(&pool)
    .await
    .expect("create commit table");
    let track_id = TrackId::from("track-1");
    for index in 0..4_i64 {
        sqlx::query(
            r#"INSERT INTO track_vcs_commits (
                   hash, track_id, parent_hash, tree_hash, manifest_schema_version,
                   author, message, event_id, created_at
               )
               VALUES (?1, ?2, NULL, ?3, ?4, NULL, 'seed', ?5, ?5)"#,
        )
        .bind(format!("{index:064x}"))
        .bind(track_id.as_str())
        .bind(format!("tree-{index}"))
        .bind(MANIFEST_SCHEMA_VERSION)
        .bind(index)
        .execute(&pool)
        .await
        .expect("insert seed commit");
    }

    let first = commit_records_for_track_pool(&pool, &track_id, 2, true, None)
        .await
        .expect("first log page");
    assert_eq!(
        first
            .iter()
            .map(|record| record.created_at)
            .collect::<Vec<_>>(),
        vec![3, 2]
    );

    sqlx::query(
        r#"INSERT INTO track_vcs_commits (
               hash, track_id, parent_hash, tree_hash, manifest_schema_version,
               author, message, event_id, created_at
           )
           VALUES (?1, ?2, NULL, 'tree-new', ?3, NULL, 'concurrent', 10, 10)"#,
    )
    .bind(format!("{:064x}", 10))
    .bind(track_id.as_str())
    .bind(MANIFEST_SCHEMA_VERSION)
    .execute(&pool)
    .await
    .expect("insert concurrent commit");

    let second = commit_records_for_track_pool(&pool, &track_id, 2, true, first.last())
        .await
        .expect("second log page");
    assert_eq!(
        second
            .iter()
            .map(|record| record.created_at)
            .collect::<Vec<_>>(),
        vec![1, 0],
        "the second page must continue below its keyset anchor"
    );
}

#[tokio::test]
async fn forge_pr_merged_only_batch_does_not_advance_head() {
    let repo = SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open sqlite repo");
    let area = repo
        .area_create(
            calm_types::organization::LEGACY_OWNER_USER_ID,
            NewArea {
                name: "area".into(),
                color: "#336699".into(),
                sort: None,
            },
        )
        .await
        .expect("create area");
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id,
            title: "track".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .expect("create track");
    let before = head(repo.pool(), &track.id).await.expect("head before");

    let event = Event::ForgePrMerged {
        track_id: track.id.clone(),
        subject: ForgeMergeSubject { pr_number: 760 },
        head_sha: "head-sha".into(),
        merge_sha: "merge-sha".into(),
    };
    let mut tx = begin_immediate_tx(repo.pool())
        .await
        .expect("begin transaction");
    let committed = commit_events_with_author_in_tx(
        &mut tx,
        &track.id,
        Some(&ActorId::KernelDispatcher),
        42,
        &[event],
        MANIFEST_SCHEMA_VERSION,
    )
    .await
    .expect("commit forge.pr.merged batch");
    tx.commit().await.expect("commit transaction");

    let after = head(repo.pool(), &track.id).await.expect("head after");
    let commit_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM track_vcs_commits WHERE track_id = ?1")
            .bind(track.id.as_str())
            .fetch_one(repo.pool())
            .await
            .expect("commit count");
    assert_eq!(committed, None);
    assert_eq!(after, before);
    assert_eq!(commit_count, 0);
}

#[tokio::test]
async fn worktree_committed_only_batch_does_not_advance_head() {
    let repo = SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open sqlite repo");
    let area = repo
        .area_create(
            calm_types::organization::LEGACY_OWNER_USER_ID,
            NewArea {
                name: "area".into(),
                color: "#336699".into(),
                sort: None,
            },
        )
        .await
        .expect("create area");
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id,
            title: "track".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .expect("create track");
    let before = head(repo.pool(), &track.id).await.expect("head before");

    let event = Event::WorktreeCommitted {
        track_id: track.id.clone(),
        card_id: CardId::from("card-1"),
        commit_sha: "1111111111111111111111111111111111111111".into(),
        branch: "neige/track/card-1".into(),
        delivery_id: None,
        base_is_ancestor: None,
    };
    let mut tx = begin_immediate_tx(repo.pool())
        .await
        .expect("begin transaction");
    let committed = commit_events_with_author_in_tx(
        &mut tx,
        &track.id,
        Some(&ActorId::KernelDispatcher),
        42,
        &[event],
        MANIFEST_SCHEMA_VERSION,
    )
    .await
    .expect("commit worktree.committed batch");
    tx.commit().await.expect("commit transaction");

    let after = head(repo.pool(), &track.id).await.expect("head after");
    let commit_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM track_vcs_commits WHERE track_id = ?1")
            .bind(track.id.as_str())
            .fetch_one(repo.pool())
            .await
            .expect("commit count");
    assert_eq!(committed, None);
    assert_eq!(after, before);
    assert_eq!(commit_count, 0);
}

#[tokio::test]
async fn ask_batch_does_not_advance_head() {
    let repo = SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open sqlite repo");
    let area = repo
        .area_create(
            calm_types::organization::LEGACY_OWNER_USER_ID,
            NewArea {
                name: "area".into(),
                color: "#336699".into(),
                sort: None,
            },
        )
        .await
        .expect("create area");
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id,
            title: "track".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .expect("create track");
    let before = head(repo.pool(), &track.id).await.expect("head before");

    let events = vec![
        Event::AskRequested {
            action: None,
            track_id: track.id.clone(),
            questions: vec![calm_types::event::AskQuestion {
                title: "Merge PR #1?".into(),
                options: vec!["Merge".into(), "Hold".into()],
            }],
            delivery: calm_types::event::AskDelivery::Hold,
            source_item_id: None,
        },
        Event::AskAnswered {
            ask_id: 1,
            track_id: track.id.clone(),
            answers: vec![calm_types::event::AskAnswer::Option(0)],
        },
        Event::AskWithdrawn {
            ask_id: 1,
            track_id: track.id.clone(),
        },
    ];
    let mut tx = begin_immediate_tx(repo.pool())
        .await
        .expect("begin transaction");
    let committed = commit_events_with_author_in_tx(
        &mut tx,
        &track.id,
        Some(&ActorId::AiPlanner(CardId::from("planner-card"))),
        42,
        &events,
        MANIFEST_SCHEMA_VERSION,
    )
    .await
    .expect("commit ask batch");
    tx.commit().await.expect("commit transaction");

    let after = head(repo.pool(), &track.id).await.expect("head after");
    let commit_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM track_vcs_commits WHERE track_id = ?1")
            .bind(track.id.as_str())
            .fetch_one(repo.pool())
            .await
            .expect("commit count");
    assert_eq!(committed, None);
    assert_eq!(after, before);
    assert_eq!(commit_count, 0);
}

#[test]
fn track_history_pruner_config_from_env_respects_disable_and_defaults() {
    let saved_interval = std::env::var(TRACK_HISTORY_PRUNE_INTERVAL_SECS_ENV).ok();
    let saved_keep = std::env::var(TRACK_HISTORY_PRUNE_KEEP_ENV).ok();
    fn set(key: &str, value: &str) {
        // SAFETY: this test owns the track-pruner env vars it mutates.
        unsafe { std::env::set_var(key, value) };
    }
    fn remove(key: &str) {
        // SAFETY: see `set`.
        unsafe { std::env::remove_var(key) };
    }

    remove(TRACK_HISTORY_PRUNE_INTERVAL_SECS_ENV);
    remove(TRACK_HISTORY_PRUNE_KEEP_ENV);
    assert_eq!(
        track_history_pruner_config_from_env(),
        Some((
            TRACK_HISTORY_PRUNE_INTERVAL,
            DEFAULT_TRACK_HISTORY_PRUNE_KEEP
        ))
    );

    set(TRACK_HISTORY_PRUNE_INTERVAL_SECS_ENV, "0");
    assert_eq!(track_history_pruner_config_from_env(), None);

    set(TRACK_HISTORY_PRUNE_INTERVAL_SECS_ENV, "17");
    set(TRACK_HISTORY_PRUNE_KEEP_ENV, "23");
    assert_eq!(
        track_history_pruner_config_from_env(),
        Some((Duration::from_secs(17), 23))
    );

    set(TRACK_HISTORY_PRUNE_KEEP_ENV, "0");
    assert_eq!(
        track_history_pruner_config_from_env(),
        Some((Duration::from_secs(17), DEFAULT_TRACK_HISTORY_PRUNE_KEEP))
    );

    match saved_interval {
        Some(value) => set(TRACK_HISTORY_PRUNE_INTERVAL_SECS_ENV, &value),
        None => remove(TRACK_HISTORY_PRUNE_INTERVAL_SECS_ENV),
    }
    match saved_keep {
        Some(value) => set(TRACK_HISTORY_PRUNE_KEEP_ENV, &value),
        None => remove(TRACK_HISTORY_PRUNE_KEEP_ENV),
    }
}

type CasObjectRow = (String, String, Vec<u8>, i64);
type CasRefRow = (String, String, Option<i64>);
type CasCommitRow = (
    String,
    String,
    Option<String>,
    String,
    i64,
    Option<String>,
    Option<String>,
    Option<i64>,
    i64,
);

#[derive(Debug, PartialEq)]
struct CasState {
    objects: Vec<CasObjectRow>,
    refs: Vec<CasRefRow>,
    commits: Vec<CasCommitRow>,
}

async fn cas_state(pool: &sqlx::SqlitePool) -> CasState {
    CasState {
        objects: sqlx::query_as(
            "SELECT hash,kind,bytes,created_at FROM track_vcs_objects ORDER BY hash",
        )
        .fetch_all(pool)
        .await
        .unwrap(),
        refs: sqlx::query_as(
            "SELECT track_id,head_hash,updated_event_id FROM track_vcs_refs ORDER BY track_id",
        )
        .fetch_all(pool)
        .await
        .unwrap(),
        commits: sqlx::query_as(
            "SELECT hash,track_id,parent_hash,tree_hash,manifest_schema_version,
                    author,message,event_id,created_at FROM track_vcs_commits ORDER BY hash",
        )
        .fetch_all(pool)
        .await
        .unwrap(),
    }
}

#[tokio::test]
async fn unchanged_public_snapshot_does_not_attempt_existing_cas_object_inserts() {
    let f = super::transcript_writer_tests::Fixture::new().await;
    let mut tx = begin_immediate_tx(f.repo.pool()).await.unwrap();
    let warm = snapshot_tree(&mut tx, &f.track.id, MANIFEST_SCHEMA_VERSION)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let before = cas_state(f.repo.pool()).await;
    assert!(!before.refs.is_empty());
    assert!(!before.commits.is_empty());
    assert!(before.objects.iter().any(|(_, kind, _, _)| kind == "tree"));
    assert!(before.objects.iter().any(|(_, kind, _, _)| kind == "blob"));
    let mut tx = begin_immediate_tx(f.repo.pool()).await.unwrap();
    // This isolated test trigger observes actual source-row attempts, including ignored
    // existing hashes. It does not replace the owner or reproduce its insertion policy.
    sqlx::query("CREATE TABLE cas_insert_probe (kind TEXT NOT NULL)")
        .execute(&mut **tx)
        .await
        .unwrap();
    sqlx::query(
        "CREATE TRIGGER cas_insert_probe BEFORE INSERT ON track_vcs_objects
         BEGIN INSERT INTO cas_insert_probe(kind) VALUES(NEW.kind); END",
    )
    .execute(&mut **tx)
    .await
    .unwrap();
    let repeated = snapshot_tree(&mut tx, &f.track.id, MANIFEST_SCHEMA_VERSION)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    // Pin all semantics before the cost oracle: object identity/body/type/age, HEAD refs
    // and every commit field remain unchanged. This is a snapshot, not a new checkpoint.
    assert_eq!(warm.tree_hash, repeated.tree_hash);
    assert_eq!(warm.manifest, repeated.manifest);
    assert_eq!(cas_state(f.repo.pool()).await, before);
    let attempts: Vec<(String, i64)> =
        sqlx::query_as("SELECT kind,count(*) FROM cas_insert_probe GROUP BY kind ORDER BY kind")
            .fetch_all(f.repo.pool())
            .await
            .unwrap();
    let total: i64 = attempts.iter().map(|(_, count)| count).sum();
    assert_eq!(
        total, 0,
        "unchanged CAS insert attempts by kind: {attempts:?}"
    );
}

#[tokio::test]
async fn immutable_cas_keeps_existing_facts_and_guards_real_new_row_attempts() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let hash = put_blob(&mut tx, "blob", b"unchanged typed CAS input")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    // Explicit raw corruption is not repaired by a typed CAS no-op, just as with IGNORE.
    sqlx::query("UPDATE track_vcs_objects SET kind='tree',bytes=X'00',created_at=17 WHERE hash=?1")
        .bind(&hash)
        .execute(repo.pool())
        .await
        .unwrap();
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    let again = put_blob(&mut tx, "blob", b"unchanged typed CAS input")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(again, hash);
    let facts: (String, Vec<u8>, i64) =
        sqlx::query_as("SELECT kind,bytes,created_at FROM track_vcs_objects WHERE hash=?1")
            .bind(&hash)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(facts, ("tree".into(), vec![0], 17));
    // Isolated fault injection: supported DML cannot delete required epoch state.
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    sqlx::query("DROP TRIGGER frontier_state_no_delete")
        .execute(&mut **tx)
        .await
        .unwrap();
    sqlx::query("DELETE FROM track_vcs_gc_frontier_state")
        .execute(&mut **tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    assert!(
        put_blob(&mut tx, "blob", b"actual new CAS fact")
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    assert!(
        sqlx::query(
            "INSERT OR REPLACE INTO track_vcs_objects(hash,kind,bytes,created_at)
         VALUES(?1,'blob',X'01',19)",
        )
        .bind(&hash)
        .execute(repo.pool())
        .await
        .is_err()
    );
    let after: (String, Vec<u8>, i64) =
        sqlx::query_as("SELECT kind,bytes,created_at FROM track_vcs_objects WHERE hash=?1")
            .bind(&hash)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(after, facts);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM track_vcs_objects")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
}
