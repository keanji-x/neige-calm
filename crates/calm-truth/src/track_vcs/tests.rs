use super::gc::{
    DEFAULT_TRACK_HISTORY_PRUNE_KEEP, TRACK_HISTORY_PRUNE_INTERVAL,
    TRACK_HISTORY_PRUNE_INTERVAL_SECS_ENV, TRACK_HISTORY_PRUNE_KEEP_ENV,
    track_history_pruner_config_from_env,
};
use super::store::{CommitTreeMeta, commit_hash_for_tree};
use super::*;
use crate::db::prelude::*;
use crate::db::sqlite::{SqlxRepo, begin_immediate_tx};
use crate::event::{Event, ForgeMergeSubject};
use crate::ids::{ActorId, CardId, TrackId};
use crate::model::{NewArea, NewTrack, RequestTheme};
use calm_types::event::{ChannelVerdict, ChannelVerdictKind, RatifyDecision, ReviewSubject};
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
        Some(&ActorId::AiSpec(CardId::from("spec-card"))),
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
