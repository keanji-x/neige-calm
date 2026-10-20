//! #2774 at the owners: the transcript write fence and the conversation floors, on the real schema.
use super::session_conversation_tests::card;
use super::*;
use crate::db::{RepoOutOfDomain, RepoRead, TranscriptReceiptItem};
use crate::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};

struct Fixture {
    repo: SqlxRepo,
    card: String,
    track: String,
    other_card: String,
    other_track: String,
}

/// Start `id` as the card's current session (superseding the previous one), as a start does.
async fn start(repo: &SqlxRepo, card: &str, id: &str) {
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    if let Some(active) = session_projection_active_for_card_tx(&mut tx, card)
        .await
        .unwrap()
    {
        session_supersede_active_tx(&mut tx, &active.id, 1)
            .await
            .unwrap();
    }
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: id.into(),
            card_id: card.into(),
            kind: WorkerSessionKind::CodexCard,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some("thread".into()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: 1,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

async fn fixture() -> Fixture {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let (card, track) = card(&repo).await;
    let (other_card, other_track) = super::session_conversation_tests::card(&repo).await;
    start(&repo, &card, "old").await;
    Fixture {
        repo,
        card,
        track,
        other_card,
        other_track,
    }
}

impl Fixture {
    async fn insert(&self, worker: &str, card: &str, track: &str, client: &str) -> Option<i64> {
        self.repo
            .harness_item_insert(
                worker,
                card,
                track,
                "thread",
                None,
                Some(client),
                Some("userMessage"),
                "item/completed",
                "{}",
                None,
            )
            .await
            .unwrap()
    }

    async fn rows(&self) -> Vec<(i64, Option<String>, Option<String>)> {
        sqlx::query_as(
            "SELECT id, turn_id, item_uuid FROM harness_items WHERE card_id=?1 ORDER BY id",
        )
        .bind(&self.card)
        .fetch_all(self.repo.db().read_pool())
        .await
        .unwrap()
    }

    /// Every way a writer stops being the card's current session, or never was.
    async fn stale_callers(&self) -> Vec<(&'static str, String, String, String)> {
        vec![
            (
                "superseded",
                "old".into(),
                self.card.clone(),
                self.track.clone(),
            ),
            (
                "wrong card",
                "new".into(),
                self.other_card.clone(),
                self.track.clone(),
            ),
            (
                "wrong track",
                "new".into(),
                self.card.clone(),
                self.other_track.clone(),
            ),
            (
                "unknown session",
                "nobody".into(),
                self.card.clone(),
                self.track.clone(),
            ),
        ]
    }
}

#[tokio::test]
async fn planner_new_conversation_fence_drops_every_transcript_write_of_a_non_current_session() {
    let f = fixture().await;
    let projection = f.insert("old", &f.card, &f.track, "p1").await.unwrap();
    let outcome_before = f
        .repo
        .harness_turn_outcome_put("old", &f.card, &f.track, "thread", "t0", "{}")
        .await
        .unwrap()
        .expect("the current session records its outcome");
    start(&f.repo, &f.card, "new").await;
    let image = f.rows().await;
    for (case, worker, card, track) in f.stale_callers().await {
        assert_eq!(
            f.insert(&worker, &card, &track, "late").await,
            None,
            "{case}"
        );
        assert_eq!(
            f.repo
                .transcript_projection_upgrade(&worker, &card, &track, "p1", Some("t"), "x", "{}")
                .await
                .unwrap(),
            None,
            "{case}: upgrade"
        );
        assert_eq!(
            f.repo
                .transcript_projection_delete(&worker, &card, &track, "p1")
                .await
                .unwrap(),
            0,
            "{case}: delete"
        );
        let mut tx = begin_immediate_tx(f.repo.db()).await.unwrap();
        assert_eq!(
            transcript_projection_delete_tx(&mut tx, &worker, &card, &track, "p1")
                .await
                .unwrap(),
            0,
            "{case}: delete in a transaction"
        );
        assert_eq!(
            transcript_delete_thread_suffix_tx(
                &mut tx,
                &worker,
                &card,
                &track,
                "thread",
                0,
                i64::MAX
            )
            .await
            .unwrap(),
            0,
            "{case}: suffix delete"
        );
        tx.commit().await.unwrap();
        assert_eq!(
            f.repo
                .harness_turn_outcome_put(&worker, &card, &track, "thread", "t-late", "{}")
                .await
                .unwrap(),
            None,
            "{case}: outcome"
        );
        f.repo
            .transcript_receipt_restore(
                &worker,
                &card,
                &track,
                "thread",
                "t-late",
                &[TranscriptReceiptItem {
                    item_uuid: "r1".into(),
                    item_type: "agentMessage".into(),
                    method: "item/completed".into(),
                    params: "{}".into(),
                }],
            )
            .await
            .unwrap();
        assert_eq!(f.rows().await, image, "{case}: nothing changed");
    }
    assert_eq!(image.len(), 2, "{image:?}");
    assert_eq!(image[0].0, projection);
    assert_eq!(image[1].0, outcome_before);

    // The current session writes, and may still settle the predecessor's card-scoped projection.
    assert!(f.insert("new", &f.card, &f.track, "p2").await.is_some());
    assert_eq!(
        f.repo
            .transcript_projection_upgrade("new", &f.card, &f.track, "p1", Some("t"), "x", "{}")
            .await
            .unwrap(),
        Some(projection)
    );
    // A current failed owner may still settle.
    sqlx::query("UPDATE worker_sessions SET state='failed' WHERE id='new'")
        .execute(f.repo.db().writer_pool())
        .await
        .unwrap();
    assert!(
        f.repo
            .harness_turn_outcome_put("new", &f.card, &f.track, "thread", "t1", "{}")
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn planner_new_conversation_fence_drops_a_write_after_the_card_is_repointed() {
    let f = fixture().await;
    // The row stays non-superseded, but the card now names another (failed) session.
    start(&f.repo, &f.card, "other").await;
    for (id, state) in [("other", "failed"), ("old", "idle")] {
        sqlx::query("UPDATE worker_sessions SET state=?2 WHERE id=?1")
            .bind(id)
            .bind(state)
            .execute(f.repo.db().writer_pool())
            .await
            .unwrap();
    }
    assert_eq!(f.insert("old", &f.card, &f.track, "late").await, None);
    assert!(f.insert("other", &f.card, &f.track, "now").await.is_some());
}

#[tokio::test]
async fn planner_new_conversation_floors_bound_every_conversation() {
    let f = fixture().await;
    let first = f.insert("old", &f.card, &f.track, "a").await.unwrap();
    let mut tx = begin_immediate_tx(f.repo.db()).await.unwrap();
    assert_eq!(
        planner_conversation_start_tx(&mut tx, &f.card, 10)
            .await
            .unwrap(),
        1
    );
    tx.commit().await.unwrap();
    // An empty conversation in between is never listed.
    let mut tx = begin_immediate_tx(f.repo.db()).await.unwrap();
    assert_eq!(
        planner_conversation_start_tx(&mut tx, &f.card, 20)
            .await
            .unwrap(),
        2
    );
    tx.commit().await.unwrap();
    let current = f.insert("old", &f.card, &f.track, "b").await.unwrap();

    let page = |conversation| {
        let repo = &f.repo;
        let card = f.card.clone();
        async move {
            repo.planner_conversation_transcript(&card, conversation, 0, 100, false)
                .await
                .unwrap()
                .map(|(_, rows)| rows.into_iter().map(|row| row.id).collect::<Vec<_>>())
        }
    };
    assert_eq!(
        page(None).await,
        Some(vec![current]),
        "the current one by default"
    );
    assert_eq!(page(Some(2)).await, Some(vec![current]));
    assert_eq!(page(Some(0)).await, Some(vec![first]));
    assert_eq!(page(Some(1)).await, Some(vec![]));
    assert_eq!(page(Some(3)).await, None, "no such conversation");
    assert_eq!(page(Some(-1)).await, None);
    let newest = f.insert("old", &f.card, &f.track, "c").await.unwrap();
    for (after, limit, descending, expected) in [
        (0, 1, false, vec![current]),
        (0, 1, true, vec![newest]),
        (newest, 1, true, vec![current]),
        (current, 1, false, vec![newest]),
        (0, 0, true, vec![]),
        (newest, 1, false, vec![]),
    ] {
        let (ordinal, rows) = f
            .repo
            .planner_conversation_transcript(&f.card, None, after, limit, descending)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(ordinal, 2, "empty pages also preserve their identity");
        assert_eq!(
            rows.into_iter().map(|row| row.id).collect::<Vec<_>>(),
            expected
        );
    }
    let history = f.repo.planner_conversations_earlier(&f.card).await.unwrap();
    assert_eq!(
        history
            .iter()
            .map(|c| (c.ordinal, c.ended_at_ms))
            .collect::<Vec<_>>(),
        vec![(0, 10)],
        "conversation 1 holds no row; the current one is not history"
    );
    // A card without floors reads its whole transcript as conversation 0.
    let other = f
        .repo
        .planner_conversation_transcript(&f.other_card, None, 0, 100, false)
        .await
        .unwrap();
    assert_eq!(
        other.map(|(ordinal, rows)| {
            assert_eq!(ordinal, 0);
            rows.len()
        }),
        Some(0)
    );
    assert!(
        f.repo
            .planner_conversations_earlier(&f.other_card)
            .await
            .unwrap()
            .is_empty()
    );
}

#[cfg(feature = "fixtures")]
#[tokio::test]
async fn planner_new_conversation_transcript_reads_one_snapshot_during_cutover() {
    use crate::test_seam::{PausePoint, install_pause_for_test};
    use std::sync::Arc;
    // A WAL-backed database allows a real cutover to commit while the read is paused.
    let dir = tempfile::tempdir().unwrap();
    let repo = Arc::new(
        SqlxRepo::open(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("snapshot.db").display()
        ))
        .await
        .unwrap(),
    );
    let (card, track) = card(&repo).await;
    start(&repo, &card, "snapshot-writer").await;
    let insert = |client: &'static str| {
        let repo = repo.clone();
        let card = card.clone();
        let track = track.clone();
        async move {
            repo.harness_item_insert(
                "snapshot-writer",
                &card,
                &track,
                "thread",
                None,
                Some(client),
                Some("agentMessage"),
                "item/completed",
                "{}",
                None,
            )
            .await
            .unwrap()
            .unwrap()
        }
    };
    let mut expected = vec![insert("before").await];
    for conversation in [None, Some(1)] {
        let pause = PausePoint::default();
        install_pause_for_test("planner_conversation_range_read", &card, pause.clone());
        let reader = {
            let repo = repo.clone();
            let card = card.clone();
            tokio::spawn(async move {
                repo.planner_conversation_transcript(&card, conversation, 0, 100, false)
                    .await
                    .unwrap()
                    .unwrap()
            })
        };
        pause.entered.notified().await;
        let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
        planner_conversation_start_tx(&mut tx, &card, 10)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let new = insert(if conversation.is_none() {
            "after-first"
        } else {
            "after-second"
        })
        .await;
        pause.release.notify_one();
        let (ordinal, rows) = reader.await.unwrap();
        assert_eq!(ordinal, conversation.unwrap_or(0));
        assert_eq!(
            rows.iter().map(|row| row.id).collect::<Vec<_>>(),
            expected,
            "a concurrent cutover cannot mix the old range with new rows"
        );
        expected = vec![new];
    }
}
