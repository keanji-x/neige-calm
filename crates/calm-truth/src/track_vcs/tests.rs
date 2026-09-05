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
use calm_types::event::{ChannelVerdict, ChannelVerdictKind, RatifyDecision, ReviewSubject};
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
        commit_hash_for_tree(&track_id, "tree-1", "draft", &base).unwrap(),
        commit_hash_for_tree(&track_id, "tree-1", "draft", &other_author).unwrap()
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
               lifecycle TEXT NOT NULL,
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
                   author, message, lifecycle, event_id, created_at
               )
               VALUES (?1, ?2, NULL, ?3, ?4, NULL, 'seed', 'active', ?5, ?5)"#,
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
               author, message, lifecycle, event_id, created_at
           )
           VALUES (?1, ?2, NULL, 'tree-new', ?3, NULL, 'concurrent',
                   'active', 10, 10)"#,
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
        .area_create(NewArea {
            name: "area".into(),
            color: "#336699".into(),
            sort: None,
        })
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
        subject: ForgeMergeSubject {
            phase: "impl".into(),
            slice_id: "6".into(),
            pr_number: 760,
        },
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
        .area_create(NewArea {
            name: "area".into(),
            color: "#336699".into(),
            sort: None,
        })
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
async fn review_ratify_only_batch_does_not_advance_head() {
    let repo = SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open sqlite repo");
    let area = repo
        .area_create(NewArea {
            name: "area".into(),
            color: "#336699".into(),
            sort: None,
        })
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
        Event::ReviewRound {
            track_id: track.id.clone(),
            subject: ReviewSubject {
                phase: "impl".into(),
                slice_id: "5b".into(),
                pr_number: Some(760),
            },
            head_sha: Some("head-sha".into()),
            n: 1,
            cap: 8,
            converged: false,
            channels: vec![
                ChannelVerdict {
                    role: "design-correctness".into(),
                    verdict: ChannelVerdictKind::ChangesRequested,
                },
                ChannelVerdict {
                    role: "failure-path".into(),
                    verdict: ChannelVerdictKind::Approved,
                },
            ],
            root_cause: Some("tests failing".into()),
            idempotency_key: format!("review.round:{}:impl:5b:760:1", track.id),
        },
        Event::RatifyRequested {
            track_id: track.id.clone(),
            reason: "cap_exhausted".into(),
        },
        Event::RatifyResolved {
            track_id: track.id.clone(),
            decision: RatifyDecision::Grant,
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
    .expect("commit review/ratify batch");
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
