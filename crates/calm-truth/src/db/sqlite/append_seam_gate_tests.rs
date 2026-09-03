//! #1252 S3′ — the append seam actually consults the role gate.
//!
//! ## What these tests are for, and what they are NOT evidence of
//!
//! `append_decision_event_in_tx` / `append_decision_events_in_tx` now mint a
//! `gated::Authorized` capability by running the role gate against the live
//! `cards` / `tracks` rows in the caller's transaction. If `authorize` ever
//! swallowed the `RoleViolation` — `Err(_) => Ok(())` — nothing else in the
//! tree would notice, because **every triple that reaches this seam in
//! production is one the gate accepts**. That is the whole reason this file
//! exists: it is the only thing standing between a swallowed violation and a
//! silently ungated append.
//!
//! ### The actors below are not production-reachable at this seam
//!
//! Stated plainly, because a test that implied otherwise would be worse than
//! no test at all. The fifteen production call sites of these two appenders
//! reach them with exactly three actor shapes:
//!
//!   * `ActorId::KernelDispatcher` — literal, at seven of them;
//!   * `ActorId::Kernel` — from `child_track_adapter`'s projected kernel
//!     events;
//!   * `ActorId::User` — the only non-degenerate output of
//!     `calm_server::actor::Actor::to_actor_id`.
//!
//! `enforce_role` lets all three through on every arm those call sites can
//! reach. So there is no production input that this seam rejects today, and
//! these tests do not claim there is. They construct actor/event pairs that
//! the gate *does* reject in order to prove the seam is wired to the gate at
//! all — the property that makes the *next* `role_gate` rule apply here
//! without re-auditing fifteen call sites.
//!
//! Concretely non-production-reachable here:
//!
//!   * `ActorId::AiCodex(CardId(""))` — `Actor::to_actor_id` can mint this
//!     from the legacy `X-Calm-Actor: ai:codex` header, but no call site of
//!     these two appenders passes a header-derived actor through unmodified.
//!   * `ActorId::User` + `Event::TaskContextFrozen` — `role_gate.rs`'s
//!     kernel-only arm denies it; the production emitter of that event is the
//!     scheduler, which is `KernelDispatcher`.
//!   * `ActorId::AiCodex(worker_card)` with a foreign `scope.track` — the
//!     #232 scope-spoof shape. No appender call site builds a scope this way.

use super::{
    SqlxRepo, append_decision_event_in_tx, append_decision_events_in_tx, area_create_tx,
    begin_immediate_tx, card_create_with_id_tx, track_create_tx,
};
use crate::error::CalmError;
use crate::event::{Event, EventScope};
use crate::ids::{ActorId, AreaId, CardId, TrackId};
use crate::model::{CardRole, NewArea, NewCard, NewTrack, RequestTheme};
use serde_json::json;

struct Home {
    card: CardId,
    track: TrackId,
    area: AreaId,
}

impl Home {
    fn scope(&self) -> EventScope {
        EventScope::Card {
            card: self.card.clone(),
            track: self.track.clone(),
            area: self.area.clone(),
        }
    }
}

async fn seed(repo: &SqlxRepo, label: &str, role: CardRole) -> Home {
    let mut tx = repo.pool().begin().await.expect("begin seed tx");
    let area = area_create_tx(
        &mut tx,
        NewArea {
            name: format!("s3-seam {label}"),
            color: "#101010".into(),
            sort: None,
        },
    )
    .await
    .expect("create area");
    let track = track_create_tx(
        &mut tx,
        NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: format!("s3-seam {label}"),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        None,
        &crate::db::sqlite::TrackWorkspacePlan::AttachedFromCwd,
        None,
        repo.track_area_cache(),
    )
    .await
    .expect("create track");
    let card = card_create_with_id_tx(
        &mut tx,
        format!("card-s3-seam-{label}"),
        NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1, "case": label}),
        },
        role,
        true,
        repo.card_role_cache(),
    )
    .await
    .expect("create card");
    tx.commit().await.expect("commit seed tx");
    Home {
        card: card.id,
        track: track.id,
        area: area.id,
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

fn task_context_frozen() -> Event {
    Event::TaskContextFrozen {
        track_id: TrackId::from("track-s3-seam"),
        task_key: "task-s3-seam".into(),
        idempotency_key: "task-s3-seam".into(),
        task_id: "task-s3-seam".into(),
        refs: Vec::new(),
        doc_revs: Default::default(),
        truncated: false,
    }
}

fn events_row_count(repo: &SqlxRepo) -> impl std::future::Future<Output = i64> + '_ {
    async move {
        sqlx::query_scalar("SELECT COUNT(*) FROM events")
            .fetch_one(repo.pool())
            .await
            .expect("count events")
    }
}

/// One row per `(actor, scope, event)` the gate must refuse at this seam.
///
/// Each row dies independently if `authorize` stops propagating the
/// `RoleViolation`, and each names a *different* `role_gate` arm, so a
/// narrowing mutation (one arm neutered) is caught as well as the blanket
/// `Err(_) => Ok(())` one.
#[tokio::test]
async fn append_seam_refuses_every_triple_the_role_gate_refuses() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open repo");
    let worker = seed(&repo, "worker", CardRole::Worker).await;
    let foreign = seed(&repo, "foreign", CardRole::Worker).await;

    // (a) empty-CardId AI actor — role_gate.rs section (1).
    let cases: Vec<(&str, ActorId, EventScope, Event, &str)> = vec![
        (
            "empty ai card id",
            ActorId::AiCodex(CardId::from("")),
            worker.scope(),
            codex_hook(&worker.card, "s3-seam-empty"),
            "empty",
        ),
        // (b) User forging a kernel-only record — role_gate.rs section for
        // `TaskContextFrozen`.
        (
            "user forging task.context_frozen",
            ActorId::User,
            worker.scope(),
            task_context_frozen(),
            "task.context_frozen",
        ),
        // (c) worker card writing into a foreign track's scope — #232.
        (
            "worker scope spoof",
            ActorId::AiCodex(worker.card.clone()),
            EventScope::Card {
                card: worker.card.clone(),
                track: foreign.track.clone(),
                area: foreign.area.clone(),
            },
            codex_hook(&worker.card, "s3-seam-spoof"),
            "scope.track mismatch",
        ),
        // (d) AI worker actor naming a card that does not exist — the
        // unknown-card deny. This is also the row that proves the gate reads
        // the *transaction*, not a cache: nothing ever inserted this id.
        (
            "unknown card",
            ActorId::AiCodex(CardId::from("card-s3-seam-never-minted")),
            worker.scope(),
            codex_hook(&worker.card, "s3-seam-unknown"),
            "does not know",
        ),
    ];

    for (label, actor, scope, event, expected) in cases {
        let before = events_row_count(&repo).await;
        let mut tx = begin_immediate_tx(repo.pool()).await.expect("begin tx");
        let err = append_decision_event_in_tx(&mut tx, &actor, &scope, None, &event)
            .await
            .expect_err(&format!("{label}: seam must refuse"));
        match &err {
            CalmError::Forbidden(message) => assert!(
                message.contains(expected),
                "{label}: expected a violation mentioning {expected:?}, got {message}"
            ),
            other => panic!("{label}: expected Forbidden, got {other:?}"),
        }
        drop(tx);
        assert_eq!(
            events_row_count(&repo).await,
            before,
            "{label}: refused append must not have written an events row"
        );
    }
}

/// The batch appender runs the gate per event, not once for the batch: a
/// refused event in the middle stops the batch and leaves no row for it.
#[tokio::test]
async fn batch_append_seam_gates_each_event_and_writes_none_on_refusal() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open repo");
    let worker = seed(&repo, "batch", CardRole::Worker).await;
    let before = events_row_count(&repo).await;

    let events = vec![
        codex_hook(&worker.card, "s3-seam-batch-ok"),
        task_context_frozen(),
    ];
    let mut tx = begin_immediate_tx(repo.pool()).await.expect("begin tx");
    let err = append_decision_events_in_tx(&mut tx, &ActorId::User, &worker.scope(), None, &events)
        .await
        .expect_err("batch must refuse the kernel-only event");
    match &err {
        CalmError::Forbidden(message) => {
            assert!(message.contains("task.context_frozen"), "{message}");
        }
        other => panic!("expected Forbidden, got {other:?}"),
    }
    // The transaction is dropped without commit, so neither the accepted first
    // event nor the refused second one survives.
    drop(tx);
    assert_eq!(events_row_count(&repo).await, before);
}

/// The seam is not a *new* gate: the triples the fifteen production call sites
/// actually pass still go through. This is the "plumbing, not a gate" claim
/// stated as a test, so a future tightening of `enforce_role` that would break
/// a production caller shows up here rather than in production.
#[tokio::test]
async fn append_seam_admits_the_actor_shapes_production_call_sites_use() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open repo");
    let worker = seed(&repo, "admit", CardRole::Worker).await;

    for actor in [ActorId::KernelDispatcher, ActorId::Kernel, ActorId::User] {
        let mut tx = begin_immediate_tx(repo.pool()).await.expect("begin tx");
        let event = codex_hook(&worker.card, &format!("s3-seam-admit-{actor:?}"));
        let id = append_decision_event_in_tx(&mut tx, &actor, &worker.scope(), None, &event)
            .await
            .unwrap_or_else(|e| panic!("{actor:?} must still be admitted: {e:?}"));
        assert!(id > 0);
        tx.commit().await.expect("commit");
    }
}
