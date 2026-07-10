use super::{SqlxRepo, card_create_with_id_tx, cove_create_tx, wave_create_tx};
use crate::db::RepoEventWrite;
use crate::error::CalmError;
use crate::event::{Event, EventBus, EventScope};
use crate::ids::{ActorId, CardId, CoveId, WaveId};
use crate::model::{CardRole, NewCard, NewCove, NewWave, RequestTheme};
use crate::state::WriteContext;
use serde_json::json;

struct WorkerCardHome {
    card: CardId,
    wave: WaveId,
    cove: CoveId,
}

async fn seed_worker_card(repo: &SqlxRepo, label: &str) -> WorkerCardHome {
    let mut tx = repo.pool().begin().await.expect("begin seed tx");
    let cove = cove_create_tx(
        &mut tx,
        NewCove {
            name: format!("hp1-b-i {label}"),
            color: "#101010".into(),
            sort: None,
        },
    )
    .await
    .expect("create cove");
    let wave = wave_create_tx(
        &mut tx,
        NewWave {
            workflow_input: None,
            cove_id: cove.id.clone(),
            title: format!("hp1-b-i {label}"),
            sort: None,
            cwd: "/tmp".into(),
            workflow_id: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        repo.wave_cove_cache(),
    )
    .await
    .expect("create wave");
    let card = card_create_with_id_tx(
        &mut tx,
        format!("card-hp1-b-i-{label}"),
        NewCard {
            wave_id: wave.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1, "case": label}),
        },
        CardRole::Worker,
        true,
        repo.card_role_cache(),
    )
    .await
    .expect("create worker card");
    tx.commit().await.expect("commit seed tx");

    WorkerCardHome {
        card: card.id,
        wave: wave.id,
        cove: cove.id,
    }
}

fn card_scope(home: &WorkerCardHome) -> EventScope {
    EventScope::Card {
        card: home.card.clone(),
        wave: home.wave.clone(),
        cove: home.cove.clone(),
    }
}

fn codex_hook(card: &CardId, key: &str) -> Event {
    Event::CodexHook {
        card_id: card.clone(),
        kind: "hook.codex.permission_request".into(),
        hook_idempotency_key: key.into(),
        payload: json!({}),
    }
}

#[tokio::test]
async fn card_actor_write_path_allows_self_scope_and_denies_out_of_scope() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open repo");
    let own = seed_worker_card(&repo, "own").await;
    let other = seed_worker_card(&repo, "other").await;
    let bus = EventBus::new();
    let write = WriteContext::new(
        repo.card_role_cache().clone(),
        repo.wave_cove_cache().clone(),
    );
    let actor = ActorId::AiCodex(own.card.clone());

    let allowed_event = codex_hook(&own.card, "hp1-b-i-allow");
    let allowed_id = repo
        .write_with_event(
            actor.clone(),
            card_scope(&own),
            None,
            &bus,
            &write,
            Box::new(move |_tx| Box::pin(async move { Ok(allowed_event) })),
        )
        .await
        .expect("self-scope card actor write should pass");
    assert!(allowed_id > 0);

    let denied_scope = EventScope::Card {
        card: own.card.clone(),
        wave: other.wave.clone(),
        cove: other.cove.clone(),
    };
    let denied_event = codex_hook(&own.card, "hp1-b-i-deny");
    let denied = repo
        .write_with_event(
            actor,
            denied_scope,
            None,
            &bus,
            &write,
            Box::new(move |_tx| Box::pin(async move { Ok(denied_event) })),
        )
        .await
        .expect_err("out-of-scope card actor write should be forbidden");
    match denied {
        CalmError::Forbidden(message) => {
            assert!(message.contains("scope.wave mismatch"), "{message}");
        }
        other => panic!("expected Forbidden, got {other:?}"),
    }

    let event_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events")
        .fetch_one(repo.pool())
        .await
        .expect("count events");
    assert_eq!(event_count, 1, "denied write must roll back event append");
}
