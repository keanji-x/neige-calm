use super::store::{CommitTreeMeta, commit_hash_for_tree};
use super::*;
use crate::db::prelude::*;
use crate::db::sqlite::{SqlxRepo, begin_immediate_tx};
use crate::event::ForgeMergeSubject;
use crate::model::{NewCove, NewWave, RequestTheme};
use calm_types::event::{ChannelVerdict, ChannelVerdictKind, RatifyDecision, ReviewSubject};

#[test]
fn commit_hash_ignores_author_metadata() {
    let wave_id = WaveId::from("wave-1");
    let base = CommitTreeMeta {
        parent_hash: Some("parent-1"),
        author: Some("user"),
        event_id: Some(7),
        message: "wave.updated",
        manifest_schema_version: MANIFEST_SCHEMA_VERSION,
        created_at: 1234,
    };
    let other_author = CommitTreeMeta {
        author: Some("kernel"),
        ..base
    };

    assert_eq!(
        commit_hash_for_tree(&wave_id, "tree-1", "draft", &base).unwrap(),
        commit_hash_for_tree(&wave_id, "tree-1", "draft", &other_author).unwrap()
    );
}

#[tokio::test]
async fn forge_pr_merged_only_batch_does_not_advance_head() {
    let repo = SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open sqlite repo");
    let cove = repo
        .cove_create(NewCove {
            name: "cove".into(),
            color: "#336699".into(),
            sort: None,
        })
        .await
        .expect("create cove");
    let wave = repo
        .wave_create(NewWave {
            workflow_input: None,
            cove_id: cove.id,
            title: "wave".into(),
            sort: None,
            cwd: "/tmp".into(),
            workflow_id: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .expect("create wave");
    let before = head(repo.pool(), &wave.id).await.expect("head before");

    let event = Event::ForgePrMerged {
        wave_id: wave.id.clone(),
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
        &wave.id,
        Some(&ActorId::KernelDispatcher),
        42,
        &[event],
        MANIFEST_SCHEMA_VERSION,
    )
    .await
    .expect("commit forge.pr.merged batch");
    tx.commit().await.expect("commit transaction");

    let after = head(repo.pool(), &wave.id).await.expect("head after");
    let commit_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM wave_vcs_commits WHERE wave_id = ?1")
            .bind(wave.id.as_str())
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
    let cove = repo
        .cove_create(NewCove {
            name: "cove".into(),
            color: "#336699".into(),
            sort: None,
        })
        .await
        .expect("create cove");
    let wave = repo
        .wave_create(NewWave {
            workflow_input: None,
            cove_id: cove.id,
            title: "wave".into(),
            sort: None,
            cwd: "/tmp".into(),
            workflow_id: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .expect("create wave");
    let before = head(repo.pool(), &wave.id).await.expect("head before");

    let event = Event::WorktreeCommitted {
        wave_id: wave.id.clone(),
        card_id: CardId::from("card-1"),
        commit_sha: "1111111111111111111111111111111111111111".into(),
        branch: "neige/wave/card-1".into(),
    };
    let mut tx = begin_immediate_tx(repo.pool())
        .await
        .expect("begin transaction");
    let committed = commit_events_with_author_in_tx(
        &mut tx,
        &wave.id,
        Some(&ActorId::KernelDispatcher),
        42,
        &[event],
        MANIFEST_SCHEMA_VERSION,
    )
    .await
    .expect("commit worktree.committed batch");
    tx.commit().await.expect("commit transaction");

    let after = head(repo.pool(), &wave.id).await.expect("head after");
    let commit_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM wave_vcs_commits WHERE wave_id = ?1")
            .bind(wave.id.as_str())
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
    let cove = repo
        .cove_create(NewCove {
            name: "cove".into(),
            color: "#336699".into(),
            sort: None,
        })
        .await
        .expect("create cove");
    let wave = repo
        .wave_create(NewWave {
            workflow_input: None,
            cove_id: cove.id,
            title: "wave".into(),
            sort: None,
            cwd: "/tmp".into(),
            workflow_id: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .expect("create wave");
    let before = head(repo.pool(), &wave.id).await.expect("head before");

    let events = vec![
        Event::ReviewRound {
            wave_id: wave.id.clone(),
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
            idempotency_key: format!("review.round:{}:impl:5b:760:1", wave.id),
        },
        Event::RatifyRequested {
            wave_id: wave.id.clone(),
            reason: "cap_exhausted".into(),
        },
        Event::RatifyResolved {
            wave_id: wave.id.clone(),
            decision: RatifyDecision::Grant,
        },
    ];
    let mut tx = begin_immediate_tx(repo.pool())
        .await
        .expect("begin transaction");
    let committed = commit_events_with_author_in_tx(
        &mut tx,
        &wave.id,
        Some(&ActorId::AiSpec(CardId::from("spec-card"))),
        42,
        &events,
        MANIFEST_SCHEMA_VERSION,
    )
    .await
    .expect("commit review/ratify batch");
    tx.commit().await.expect("commit transaction");

    let after = head(repo.pool(), &wave.id).await.expect("head after");
    let commit_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM wave_vcs_commits WHERE wave_id = ?1")
            .bind(wave.id.as_str())
            .fetch_one(repo.pool())
            .await
            .expect("commit count");
    assert_eq!(committed, None);
    assert_eq!(after, before);
    assert_eq!(commit_count, 0);
}

#[test]
fn wave_history_pruner_config_from_env_respects_disable_and_defaults() {
    let saved_interval = std::env::var(WAVE_HISTORY_PRUNE_INTERVAL_SECS_ENV).ok();
    let saved_keep = std::env::var(WAVE_HISTORY_PRUNE_KEEP_ENV).ok();
    fn set(key: &str, value: &str) {
        // SAFETY: this test owns the wave-pruner env vars it mutates.
        unsafe { std::env::set_var(key, value) };
    }
    fn remove(key: &str) {
        // SAFETY: see `set`.
        unsafe { std::env::remove_var(key) };
    }

    remove(WAVE_HISTORY_PRUNE_INTERVAL_SECS_ENV);
    remove(WAVE_HISTORY_PRUNE_KEEP_ENV);
    assert_eq!(
        wave_history_pruner_config_from_env(),
        Some((WAVE_HISTORY_PRUNE_INTERVAL, DEFAULT_WAVE_HISTORY_PRUNE_KEEP))
    );

    set(WAVE_HISTORY_PRUNE_INTERVAL_SECS_ENV, "0");
    assert_eq!(wave_history_pruner_config_from_env(), None);

    set(WAVE_HISTORY_PRUNE_INTERVAL_SECS_ENV, "17");
    set(WAVE_HISTORY_PRUNE_KEEP_ENV, "23");
    assert_eq!(
        wave_history_pruner_config_from_env(),
        Some((Duration::from_secs(17), 23))
    );

    set(WAVE_HISTORY_PRUNE_KEEP_ENV, "0");
    assert_eq!(
        wave_history_pruner_config_from_env(),
        Some((Duration::from_secs(17), DEFAULT_WAVE_HISTORY_PRUNE_KEEP))
    );

    match saved_interval {
        Some(value) => set(WAVE_HISTORY_PRUNE_INTERVAL_SECS_ENV, &value),
        None => remove(WAVE_HISTORY_PRUNE_INTERVAL_SECS_ENV),
    }
    match saved_keep {
        Some(value) => set(WAVE_HISTORY_PRUNE_KEEP_ENV, &value),
        None => remove(WAVE_HISTORY_PRUNE_KEEP_ENV),
    }
}
