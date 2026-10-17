//! Deterministic drain between the metadata owner's real repository reads.
use super::*;
use crate::db::RepoOutOfDomain;
use crate::db::sqlite::{self, SqlxRepo};
use crate::harness::queue::QueueEntry;
use crate::model::{NewArea, NewCard, NewTrack, RequestTheme};
use crate::session_projection_repo::{AgentProvider, WorkerSessionInit, WorkerSessionState};
use std::sync::atomic::{AtomicBool, Ordering};

struct DrainBetweenReads {
    repo: SqlxRepo,
    card: String,
    track: String,
    drained: AtomicBool,
}

impl DrainBetweenReads {
    async fn new() -> Self {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let mut tx = repo.pool().begin().await.unwrap();
        let area = sqlite::area_create_tx(
            &mut tx,
            calm_types::organization::LEGACY_OWNER_USER_ID,
            NewArea {
                name: "titles".into(),
                color: "#101010".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
        let track = sqlite::track_create_tx(
            &mut tx,
            NewTrack {
                area_id: area.id,
                title: "unrelated".into(),
                sort: None,
                cwd: "/tmp".into(),
                template_id: None,
                plugin_scope: None,
                template_input: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            },
            None,
            &sqlite::TrackWorkspacePlan::AttachedFromCwd,
            None,
            repo.track_area_cache(),
        )
        .await
        .unwrap();
        let card = sqlite::card_create_tx(
            &mut tx,
            NewCard {
                track_id: track.id.clone(),
                kind: "codex".into(),
                sort: None,
                payload: serde_json::json!({}),
                title: None,
            },
            &crate::card_role_cache::CardRoleCache::new(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        sqlx::query("UPDATE cards SET role='assistant' WHERE id=?1")
            .bind(card.id.as_str())
            .execute(repo.pool())
            .await
            .unwrap();
        let pending = HarnessSnapshot::initial(
            0,
            vec![QueueEntry::user_message(
                "Migrating input".into(),
                None,
                Vec::new(),
            )],
        );
        let mut tx = repo.pool().begin().await.unwrap();
        sqlite::session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit::managed_assistant(
                "title-runtime".into(),
                card.id.to_string(),
                AgentProvider::Codex,
                WorkerSessionState::Idle,
                None,
                serde_json::to_value(pending).unwrap(),
                0,
            ),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        Self {
            repo,
            card: card.id.to_string(),
            track: track.id.to_string(),
            drained: AtomicBool::new(false),
        }
    }

    async fn insert(&self, text: &str) {
        self.repo
            .harness_item_insert(
                "title-runtime",
                &self.card,
                &self.track,
                "title-thread",
                None,
                None,
                Some("userMessage"),
                "item/completed",
                &serde_json::json!({"item":{"content":[{"text":text}]}}).to_string(),
                None,
            )
            .await
            .unwrap();
    }

    async fn after_read(&self) {
        if !self.drained.swap(true, Ordering::SeqCst) {
            // Match issuance's durable order: projection commits before empty pending commits.
            self.insert("Migrating input").await;
            let mut tx = self.repo.pool().begin().await.unwrap();
            assert!(
                sqlite::session_set_handle_state_tx(
                    &mut tx,
                    &"title-runtime".to_string(),
                    Some(serde_json::to_value(HarnessSnapshot::initial(0, vec![])).unwrap()),
                )
                .await
                .unwrap()
            );
            tx.commit().await.unwrap();
        }
    }
}

#[async_trait::async_trait]
impl TitleRead for DrainBetweenReads {
    async fn sources(&self, ids: &[String]) -> Result<Vec<ConversationTitleSource>> {
        let sources = self.repo.conversation_title_sources(ids).await?;
        self.after_read().await;
        Ok(sources)
    }

    async fn pending(&self, ids: &[String]) -> Result<HashMap<CardId, WorkerSessionProjection>> {
        let pending = self
            .repo
            .session_projection_projectable_for_cards(ids)
            .await?;
        self.after_read().await;
        Ok(pending)
    }
}

#[tokio::test]
async fn conversation_title_drain_between_reads_never_loses_input() {
    let reader = DrainBetweenReads::new().await;
    let ids = [reader.card.clone()];
    assert!(
        reader.repo.conversation_title_sources(&ids).await.unwrap()[0]
            .input
            .is_none()
    );
    let titles = titles(&reader, &ids).await.unwrap();
    assert!(reader.drained.load(Ordering::SeqCst));
    let runtime = reader
        .repo
        .session_projection_projectable_for_cards(&ids)
        .await
        .unwrap();
    let snapshot =
        HarnessSnapshot::parse_known(runtime[&reader.card].handle_state_json.clone().unwrap())
            .unwrap();
    assert!(snapshot.pending_entries().is_empty());
    assert_eq!(titles[&reader.card].as_deref(), Some("Migrating input"));
}

#[tokio::test]
async fn conversation_title_drain_persisted_first_input_wins_over_captured_pending() {
    let reader = DrainBetweenReads::new().await;
    reader.insert("Earlier durable input").await;
    assert_eq!(
        titles(&reader, &[reader.card.clone()]).await.unwrap()[&reader.card].as_deref(),
        Some("Earlier durable input")
    );
}
