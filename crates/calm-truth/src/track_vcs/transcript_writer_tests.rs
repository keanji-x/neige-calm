//! Pause the real transcript projection; the regression never substitutes a renderer.
use super::*;
use crate::db::prelude::*;
use crate::db::sqlite::{SqlxRepo, begin_immediate_tx};
use crate::event::{Event, EventBus, EventScope};
use crate::ids::ActorId;
use crate::model::{NewArea, NewCard, NewTrack, RequestTheme};
use calm_types::organization::LEGACY_OWNER_USER_ID;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::Notify;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Phase {
    Interpret,
    BeforeWrite,
}
pub(super) struct Pause {
    entered: Notify,
    release: Notify,
    steps: Vec<Phase>,
    next: std::sync::atomic::AtomicUsize,
    pub(super) interpretations: std::sync::atomic::AtomicUsize,
    pub(super) preparations: std::sync::atomic::AtomicUsize,
    pub(super) bindings: std::sync::atomic::AtomicUsize,
}
tokio::task_local! { static TRANSCRIPT_PAUSE: Arc<Pause>; }
pub(super) fn record_blob_binding() {
    let _ = TRANSCRIPT_PAUSE.try_with(|pause| pause.bindings.fetch_add(1, Ordering::SeqCst));
}
pub(super) async fn at_phase(phase: Phase) {
    let Ok(pause) = TRANSCRIPT_PAUSE.try_with(Arc::clone) else {
        return;
    };
    match phase {
        Phase::Interpret => {
            pause.interpretations.fetch_add(1, Ordering::SeqCst);
        }
        Phase::BeforeWrite => {
            pause.preparations.fetch_add(1, Ordering::SeqCst);
        }
    }
    let step = pause.next.load(Ordering::SeqCst);
    if pause.steps.get(step) == Some(&phase) {
        pause.next.fetch_add(1, Ordering::SeqCst);
        pause.entered.notify_one();
        pause.release.notified().await;
    }
}
pub(super) async fn pause_before_hook_interpretation() {
    at_phase(Phase::Interpret).await;
}
impl Pause {
    pub(super) async fn entered(&self) {
        tokio::time::timeout(Duration::from_secs(10), self.entered.notified())
            .await
            .unwrap();
    }
    pub(super) fn release(&self) {
        self.release.notify_one();
    }
    pub(super) fn reads(&self) -> usize {
        self.interpretations.load(Ordering::SeqCst)
    }
    pub(super) fn attempts(&self) -> usize {
        self.preparations.load(Ordering::SeqCst)
    }
    pub(super) fn binds(&self) -> usize {
        self.bindings.load(Ordering::SeqCst)
    }
}
pub(super) type Refresh = tokio::task::JoinHandle<crate::error::Result<CommitHash>>;
pub(super) fn start(
    pool: sqlx::SqlitePool,
    track: crate::ids::TrackId,
    steps: Vec<Phase>,
) -> (Arc<Pause>, Refresh) {
    let pause = Arc::new(Pause {
        entered: Notify::new(),
        release: Notify::new(),
        steps,
        next: 0.into(),
        interpretations: 0.into(),
        preparations: 0.into(),
        bindings: 0.into(),
    });
    let task = tokio::spawn(TRANSCRIPT_PAUSE.scope(pause.clone(), async move {
        snapshot_transcripts_for_cards_in_track(&pool, &track, None, MANIFEST_SCHEMA_VERSION).await
    }));
    (pause, task)
}
pub(super) async fn finish(task: Refresh) -> crate::error::Result<CommitHash> {
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap()
}
pub(super) struct Fixture {
    _directory: tempfile::TempDir,
    pub(super) repo: SqlxRepo,
    pub(super) track: crate::model::Track,
    pub(super) card: crate::model::Card,
    bus: EventBus,
}
impl Fixture {
    pub(super) async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("transcript-writer.db");
        let repo = SqlxRepo::open(&format!("sqlite://{}?mode=rwc", database.display()))
            .await
            .unwrap();
        let area = repo
            .area_create(
                LEGACY_OWNER_USER_ID,
                NewArea {
                    name: "Transcript writer regression".into(),
                    color: "#111111".into(),
                    sort: None,
                },
            )
            .await
            .unwrap();
        let track = repo
            .track_create(NewTrack {
                template_input: None,
                area_id: area.id.clone(),
                title: "Transcript refresh".into(),
                sort: None,
                cwd: "/synthetic-transcript-writer".into(),
                template_id: None,
                plugin_scope: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let card = repo
            .card_create(NewCard {
                track_id: track.id.clone(),
                kind: "codex".into(),
                sort: None,
                payload: json!({"schemaVersion": 1}),
                title: None,
            })
            .await
            .unwrap();
        let scope = EventScope::Card {
            card: card.id.clone(),
            track: track.id.clone(),
            area: area.id,
        };
        let bus = EventBus::new();
        for event in [
            Event::CardAdded(card.clone()),
            Event::CodexHook {
                card_id: card.id.clone(),
                kind: "hook.codex.user_prompt_submit".into(),
                hook_idempotency_key: "transcript-writer-hook".into(),
                payload: json!({
                    "hook_event_name": "UserPromptSubmit",
                    "prompt": "retained transcript writer probe",
                }),
            },
        ] {
            repo.log_pure_event(
                ActorId::Kernel,
                scope.clone(),
                None,
                &bus,
                repo.card_role_cache(),
                repo.track_area_cache(),
                event,
            )
            .await
            .unwrap();
        }
        Self {
            _directory: directory,
            repo,
            track,
            card,
            bus,
        }
    }
    pub(super) async fn hook(&self, prompt: &str) -> i64 {
        self.log(Event::CodexHook {
            card_id: self.card.id.clone(),
            kind: "hook.codex.user_prompt_submit".into(),
            hook_idempotency_key: prompt.into(),
            payload: json!({"hook_event_name": "UserPromptSubmit", "prompt": prompt}),
        })
        .await
    }
    pub(super) async fn log(&self, event: Event) -> i64 {
        self.repo
            .log_pure_event(
                ActorId::Kernel,
                EventScope::Card {
                    card: self.card.id.clone(),
                    track: self.track.id.clone(),
                    area: self.track.area_id.clone(),
                },
                None,
                &self.bus,
                self.repo.card_role_cache(),
                self.repo.track_area_cache(),
                event,
            )
            .await
            .unwrap()
    }
    pub(super) async fn refresh(&self) -> CommitHash {
        snapshot_transcripts_for_cards_in_track(
            self.repo.pool(),
            &self.track.id,
            None,
            MANIFEST_SCHEMA_VERSION,
        )
        .await
        .unwrap()
    }
    pub(super) async fn text(&self, commit: &str, card: &crate::ids::CardId, name: &str) -> String {
        cat_at(self.repo.pool(), commit, &format!("cards/{card}/{name}"))
            .await
            .unwrap()
            .content
    }
    pub(super) async fn refresh_count(&self) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM track_vcs_commits WHERE track_id = ?1 AND message = 'transcript refresh'")
            .bind(self.track.id.as_str()).fetch_one(self.repo.pool()).await.unwrap()
    }
}

#[tokio::test]
async fn transcript_refresh_allows_unrelated_writer_during_hook_interpretation() {
    let fx = Fixture::new().await;
    let repo = &fx.repo;
    let track = &fx.track;
    let card = &fx.card;
    sqlx::query("CREATE TABLE unrelated_transcript_writer_probe (value INTEGER NOT NULL)")
        .execute(repo.pool())
        .await
        .unwrap();
    let parent = head(repo.pool(), &track.id).await.unwrap().unwrap();
    let (pause, refresh) = start(
        repo.pool().clone(),
        track.id.clone(),
        vec![Phase::Interpret],
    );
    let entered = tokio::time::timeout(Duration::from_secs(10), pause.entered.notified()).await;
    let pool = repo.pool().clone();
    let mut writer = tokio::spawn(async move {
        let mut tx = begin_immediate_tx(&pool).await?;
        sqlx::query("INSERT INTO unrelated_transcript_writer_probe (value) VALUES (1)")
            .execute(&mut **tx)
            .await?;
        tx.commit().await?;
        Ok::<_, crate::error::TruthError>(())
    });
    let before_release = tokio::time::timeout(Duration::from_secs(1), &mut writer).await;
    // Always release and drain, including the expected old-code timeout.
    pause.release.notify_one();
    let commit = tokio::time::timeout(Duration::from_secs(10), refresh)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let writer_completed = match before_release {
        Ok(result) => {
            result.unwrap().unwrap();
            true
        }
        Err(_) => {
            tokio::time::timeout(Duration::from_secs(10), writer)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            false
        }
    };
    entered.expect("real transcript hook read reached its interpretation phase");
    assert_eq!(
        head(repo.pool(), &track.id).await.unwrap(),
        Some(commit.clone())
    );
    let record = commit_record(repo.pool(), &commit).await.unwrap().unwrap();
    assert_eq!(record.parent_hash.as_deref(), Some(parent.as_str()));
    assert_eq!(record.message.as_deref(), Some("transcript refresh"));
    for name in ["events.json", "conversation.md"] {
        let blob = cat_at(repo.pool(), &commit, &format!("cards/{}/{name}", card.id))
            .await
            .unwrap();
        assert!(blob.content.contains("retained transcript writer probe"));
    }
    assert!(
        writer_completed,
        "unrelated writer could not commit before transcript interpretation was released"
    );
}
