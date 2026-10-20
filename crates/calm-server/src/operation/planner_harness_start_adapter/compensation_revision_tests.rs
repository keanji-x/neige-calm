use super::*;
use crate::db::prelude::ServerRepoSyncDomainRawExt;
use crate::db::sqlite::SqlxRepo;
use crate::harness::queue::{QueueMutation, apply_mutation};
use crate::model::{NewArea, NewTrack};
use crate::routes::theme::RequestTheme;

fn edit(entry: QueueEntry, text: &str) -> QueueEntry {
    let actual = entry.user_ref().unwrap();
    let mut queue = std::collections::VecDeque::from([entry]);
    apply_mutation(
        &mut queue,
        &QueueMutation::Edit {
            entry_id: crate::harness::QueueEntryId::from_wire(actual.id),
            if_entry_rev: actual.rev,
            text: text.into(),
        },
    )
    .unwrap();
    queue.pop_front().unwrap()
}

struct GiveBack {
    repo: Arc<SqlxRepo>,
    card_id: String,
    source: String,
    successor: String,
    output: TxOutput,
    captured: QueueEntry,
    successor_entry: QueueEntry,
}

impl GiveBack {
    async fn seed(retain_source: bool, edit_successor: bool, exhausted: bool) -> Self {
        let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let area = repo
            .area_create(
                calm_types::organization::LEGACY_OWNER_USER_ID,
                NewArea {
                    name: "versioned compensation".into(),
                    color: "#101010".into(),
                    sort: None,
                },
            )
            .await
            .unwrap();
        let track = repo
            .track_create(NewTrack {
                area_id: area.id,
                title: "versioned compensation".into(),
                sort: None,
                cwd: String::new(),
                template_id: None,
                template_input: None,
                plugin_scope: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let captured = edit(
            QueueEntry::user_message("initial words".into(), None, Vec::new()),
            "captured words A",
        );
        let mut successor_entry = if edit_successor {
            edit(captured.clone(), "accepted successor words B")
        } else {
            captured.clone()
        };
        if exhausted {
            // A retained historical/inherited MAX is readable; compensation must not lose it.
            let QueueEntry::User { rev, .. } = &mut successor_entry else {
                unreachable!()
            };
            *rev = u32::MAX;
        }
        let source = new_id();
        let successor = new_id();
        let mut tx = repo.pool().begin().await.unwrap();
        let card = card_create_with_id_tx(
            &mut tx,
            new_id(),
            NewCard {
                track_id: track.id,
                title: None,
                kind: "codex".into(),
                sort: None,
                payload: crate::track_creation::planner_harness_card_payload(
                    &crate::acp_planner::config::AcpPlannerHost::unconfigured_scratch().unwrap(),
                    None,
                    AgentProvider::Codex,
                ),
            },
            CardRole::Planner,
            false,
            repo.card_role_cache(),
        )
        .await
        .unwrap();
        for (runtime, state, entries) in [
            (
                &source,
                WorkerSessionState::Failed,
                if retain_source {
                    vec![captured.clone()]
                } else {
                    vec![]
                },
            ),
            (
                &successor,
                WorkerSessionState::Idle,
                vec![successor_entry.clone()],
            ),
        ] {
            session_start_runtime_tx(
                &mut tx,
                WorkerSessionInit::shared_planner(
                    runtime.clone(),
                    card.id.to_string(),
                    AgentProvider::Codex,
                    state,
                    None,
                    serde_json::to_value(HarnessSnapshot::initial(0, entries)).unwrap(),
                    now_ms(),
                ),
            )
            .await
            .unwrap();
        }
        session_mark_queue_harvested_tx(&mut tx, &source, now_ms())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        // Capture through the actual producer and persisted journal, not a copied revision fixture.
        let taken = stranded_user_messages(
            &source,
            &serde_json::to_string(&HarnessSnapshot::initial(0, vec![captured.clone()])).unwrap(),
        )
        .taken;
        let mut output = TxOutput::new("card", Some(card.id.to_string()), json!({}));
        output.data = json!({"harvested_from":harvested_from_journal(&[HarvestedFrom {
            worker_session_id:source.clone(), messages:taken,
        }])});
        Self {
            repo,
            card_id: card.id.to_string(),
            source,
            successor,
            output,
            captured,
            successor_entry,
        }
    }

    async fn compensate(&self) -> Result<()> {
        let card = self.card_id.clone();
        let successor = self.successor.clone();
        let journal = read_harvested_from_journal(&self.output)?;
        write_in_tx_typed(self.repo.as_ref(), move |tx| {
            Box::pin(async move {
                return_harvested_queues_and_fail_tx(tx, &card, &successor, &journal).await
            })
        })
        .await
    }

    async fn snapshot(&self, runtime: &str) -> Value {
        let state: String =
            sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id = ?1")
                .bind(runtime)
                .fetch_one(self.repo.pool())
                .await
                .unwrap();
        serde_json::from_str(&state).unwrap()
    }

    async fn assert_later_return(&self) {
        self.compensate().await.unwrap();
        let restored =
            HarnessSnapshot::from_value_strict(self.snapshot(&self.source).await).pending_entries();
        assert_eq!(restored.len(), 1);
        let actual = restored[0].user_view().unwrap();
        let captured = self.captured.user_view().unwrap();
        let successor = self.successor_entry.user_view().unwrap();
        assert_eq!(actual.id, captured.id);
        assert_eq!(actual.text, captured.text, "existing captured TEXT policy");
        assert_eq!(restored[0].message_ids(), self.captured.message_ids());
        assert!(
            actual.rev > successor.rev,
            "restoring captured revision {} after actual successor revision {} must be a later write; got {}",
            captured.rev,
            successor.rev,
            actual.rev,
        );
        assert!(
            HarnessSnapshot::from_value_strict(self.snapshot(&self.successor).await)
                .pending_entries()
                .is_empty()
        );
        let returned = self.snapshot(&self.source).await;
        self.compensate().await.unwrap();
        assert_eq!(
            self.snapshot(&self.source).await,
            returned,
            "idempotent replay"
        );
    }
}

#[tokio::test]
async fn versioned_input_compensation_advances_past_the_edited_successor() {
    GiveBack::seed(false, true, false)
        .await
        .assert_later_return()
        .await;
}

#[tokio::test]
async fn versioned_input_compensation_advances_the_retained_source_copy() {
    GiveBack::seed(true, true, false)
        .await
        .assert_later_return()
        .await;
}

#[tokio::test]
async fn versioned_input_compensation_preserves_an_unchanged_handoff() {
    for retained in [false, true] {
        let case = GiveBack::seed(retained, false, false).await;
        case.compensate().await.unwrap();
        let restored =
            HarnessSnapshot::from_value_strict(case.snapshot(&case.source).await).pending_entries();
        assert_eq!(restored[0].user_ref(), case.captured.user_ref());
        assert_eq!(restored[0].observation(), case.captured.observation());
        assert_eq!(restored[0].message_ids(), case.captured.message_ids());
    }
}

#[tokio::test]
async fn versioned_input_compensation_exhaustion_keeps_both_queues_unchanged() {
    for retained in [false, true] {
        let case = GiveBack::seed(retained, true, true).await;
        let source = case.snapshot(&case.source).await;
        let successor = case.snapshot(&case.successor).await;
        assert!(matches!(
            case.compensate().await,
            Err(CalmError::Conflict(_))
        ));
        assert_eq!(case.snapshot(&case.source).await, source);
        assert_eq!(case.snapshot(&case.successor).await, successor);
        let state: String = sqlx::query_scalar("SELECT state FROM worker_sessions WHERE id = ?1")
            .bind(&case.successor)
            .fetch_one(case.repo.pool())
            .await
            .unwrap();
        assert_eq!(
            state, "idle",
            "the refusal rolls back the whole transaction"
        );
    }
}

#[tokio::test]
async fn versioned_input_compensation_preserves_legacy_and_refuses_malformed_journals() {
    let mut legacy = GiveBack::seed(false, false, false).await;
    legacy.output.data["harvested_from"][0]["messages"][0]
        .as_object_mut()
        .unwrap()
        .remove("entry_rev");
    legacy.compensate().await.unwrap();
    let returned =
        HarnessSnapshot::from_value_strict(legacy.snapshot(&legacy.source).await).pending_entries();
    assert_eq!(returned[0].user_ref(), None);
    assert_eq!(returned[0].observation(), legacy.captured.observation());
    assert_eq!(returned[0].message_ids(), legacy.captured.message_ids());

    let mut invalid = GiveBack::seed(true, true, false).await;
    invalid.output.data["harvested_from"][0]["messages"][0]["entry_rev"] = json!("2");
    let source = invalid.snapshot(&invalid.source).await;
    let successor = invalid.snapshot(&invalid.successor).await;
    assert!(matches!(
        invalid.compensate().await,
        Err(CalmError::Conflict(_))
    ));
    assert_eq!(invalid.snapshot(&invalid.source).await, source);
    assert_eq!(invalid.snapshot(&invalid.successor).await, successor);
}
