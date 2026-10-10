//! #2493 S1 neutrality: migration 0161 backfills each attempt's worker session from the inference
//! it replaces (the scheduler's spawn operation with `idempotency_key` = attempt, and the session
//! that operation started), on today's row shapes, through the real migrator; the binding readers
//! (the view, `worker_binding_tx`, the activity fold, checkout occupancy) then answer what the
//! inference answered. Ended sessions with surviving cards keep their history.
use std::borrow::Cow;
use std::collections::BTreeMap;

use sqlx::sqlite::SqlitePoolOptions;

use crate::db::sqlite::{
    CheckoutOccupancy, SqlxRepo, WorkerBinding, WorkerOf, checkout_occupancy, session_binding_tx,
    worker_binding_tx,
};
use crate::model::TaskStatus;
use crate::track_activity::{CardState, TrackRows, fold, sql};

const TRACK: &str = "track-backfill";
const OCCUPANCY_TRACK: &str = "track-occupancy";
const STATUSES: [TaskStatus; 6] = [
    TaskStatus::Dispatched,
    TaskStatus::Running,
    TaskStatus::Verifying,
    TaskStatus::Done,
    TaskStatus::Failed,
    TaskStatus::Canceled,
];
const SESSION_STATES: [&str; 3] = ["running", "exited", "failed"];
const KINDS: [(&str, &str, &str); 3] = [
    ("codex-worker", "codex", "codex"),
    ("claude-worker", "claude", "claude"),
    ("terminal-worker", "terminal", "terminal"),
];

/// One seeded attempt in today's shape, and what today's inference says about it.
struct Seeded {
    attempt: String,
    session: String,
    card: String,
    status: TaskStatus,
    session_active: bool,
    lease: Option<String>,
}

async fn exec(pool: &sqlx::SqlitePool, sql: &str, binds: &[&str]) {
    let mut query = sqlx::query(sql);
    for bind in binds {
        query = query.bind(*bind);
    }
    query
        .execute(pool)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

async fn seed_track(pool: &sqlx::SqlitePool, track: &str) {
    exec(
        pool,
        "INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at) VALUES(?1,'area','t',0,1,1)",
        &[track],
    )
    .await;
}

async fn seed_card(pool: &sqlx::SqlitePool, track: &str, card: &str, kind: &str) {
    exec(
        pool,
        "INSERT INTO cards(id,track_id,kind,sort,payload,title,deletable,created_at,updated_at,role) \
         VALUES(?1,?2,?3,0,'{}',NULL,1,1,1,'worker')",
        &[card, track, kind],
    )
    .await;
}

/// The scheduler's worker-spawn op for `attempt`, with the given payload actor kind.
async fn seed_op(pool: &sqlx::SqlitePool, op: &str, kind: &str, attempt: &str, actor: &str) {
    let payload = format!(r#"{{"actor":{{"kind":"{actor}"}}}}"#);
    exec(
        pool,
        "INSERT INTO operations(id,operation_key,kind,idempotency_key,payload_hash,target_type,\
         target_json,payload_json,phase,created_at_ms,updated_at_ms) \
         VALUES(?1,?1,?2,?3,'h','card','{}',?4,'succeeded',1,1)",
        &[op, kind, attempt, &payload],
    )
    .await;
}

async fn seed_session(
    pool: &sqlx::SqlitePool,
    track: &str,
    session: &str,
    provider: &str,
    state: &str,
    card: &str,
    op: Option<&str>,
) {
    sqlx::query(
        "INSERT INTO worker_sessions(id,track_id,provider,mode,contract,state,card_id,spawn_op_id,\
         created_at_ms,updated_at_ms) VALUES(?1,?2,?3,'ephemeral','executor',?4,?5,?6,7,7)",
    )
    .bind(session)
    .bind(track)
    .bind(provider)
    .bind(state)
    .bind(card)
    .bind(op)
    .execute(pool)
    .await
    .expect("seed session");
}

/// A task row as the scheduler leaves it: the card stamp is written at `running` (a dispatched
/// row is still unstamped).
async fn seed_task(
    pool: &sqlx::SqlitePool,
    track: &str,
    key: &str,
    kind: &str,
    status: TaskStatus,
    card: Option<&str>,
) -> String {
    let attempt = format!("{track}:{key}");
    let stamp = card.filter(|_| status != TaskStatus::Dispatched);
    sqlx::query(
        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,depends_on_json,status,\
         worker_card_id,finished_at_ms,created_at_ms,updated_at_ms) \
         VALUES(?1,?2,?3,?4,'g','null','[]',?5,?6,?7,1,1)",
    )
    .bind(&attempt)
    .bind(track)
    .bind(key)
    .bind(kind)
    .bind(status.wire_label())
    .bind(stamp)
    .bind(status.is_terminal().then_some(100_i64))
    .execute(pool)
    .await
    .expect("seed task");
    attempt
}

async fn seed_lease(
    pool: &sqlx::SqlitePool,
    track: &str,
    lease: &str,
    card: &str,
    op: &str,
    held: bool,
) {
    let state = if held { "held" } else { "released" };
    let path = format!("/checkout/{lease}");
    exec(
        pool,
        "INSERT INTO workspace_leases(lease_id,card_id,track_id,path,state,lease_owner,\
         created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,?6,1,1)",
        &[lease, card, track, &path, state, op],
    )
    .await;
}

#[tokio::test]
async fn worker_binding_backfill_matches_spawn_op_inference() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("old.sqlite").display()
    );
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    sqlx::query("PRAGMA foreign_keys=ON")
        .execute(&pool)
        .await
        .unwrap();
    let migrator = sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            calm_truth::MIGRATOR
                .iter()
                .filter(|m| m.version < 161)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    migrator.run(&pool).await.unwrap();
    exec(
        &pool,
        "INSERT INTO areas(id,name,color,sort,created_at,updated_at) VALUES('area','a','red',0,1,1)",
        &[],
    )
    .await;
    seed_track(&pool, TRACK).await;
    seed_track(&pool, OCCUPANCY_TRACK).await;

    let mut seeded = Vec::new();
    for (op_kind, task_kind, provider) in KINDS {
        for status in STATUSES {
            for state in SESSION_STATES {
                let key = format!("{task_kind}-{}-{state}", status.wire_label());
                let card = format!("card-{key}");
                let session = format!("session-{key}");
                let op = format!("op-{key}");
                seed_card(&pool, TRACK, &card, task_kind).await;
                let attempt = seed_task(&pool, TRACK, &key, task_kind, status, Some(&card)).await;
                seed_op(&pool, &op, op_kind, &attempt, "KernelDispatcher").await;
                seed_session(&pool, TRACK, &session, provider, state, &card, Some(&op)).await;
                let lease = (task_kind != "terminal").then(|| format!("lease-{key}"));
                if let Some(lease) = &lease {
                    let held = matches!(status, TaskStatus::Dispatched | TaskStatus::Running);
                    seed_lease(&pool, TRACK, lease, &card, &op, held).await;
                }
                seeded.push(Seeded {
                    attempt,
                    session,
                    card,
                    status,
                    session_active: state == "running",
                    lease,
                });
            }
        }
    }
    // The retired isolated kind binds as the scheduler's spawn kinds do.
    seed_card(&pool, TRACK, "card-isolated", "codex").await;
    let isolated = seed_task(
        &pool,
        TRACK,
        "isolated",
        "codex",
        TaskStatus::Failed,
        Some("card-isolated"),
    )
    .await;
    seed_op(
        &pool,
        "op-isolated",
        "codex-isolated-worker",
        &isolated,
        "KernelDispatcher",
    )
    .await;
    seed_session(
        &pool,
        TRACK,
        "session-isolated",
        "codex",
        "exited",
        "card-isolated",
        Some("op-isolated"),
    )
    .await;
    // A spawn that failed before the running stamp (4140 has one): bound, and its card stamped by
    // the backfill, so its card now shows the failed attempt.
    seed_card(&pool, TRACK, "card-spawn-failed", "codex").await;
    let spawn_failed = seed_task(
        &pool,
        TRACK,
        "spawn-failed",
        "codex",
        TaskStatus::Failed,
        None,
    )
    .await;
    seed_op(
        &pool,
        "op-spawn-failed",
        "codex-worker",
        &spawn_failed,
        "KernelDispatcher",
    )
    .await;
    seed_session(
        &pool,
        TRACK,
        "session-spawn-failed",
        "codex",
        "failed",
        "card-spawn-failed",
        Some("op-spawn-failed"),
    )
    .await;
    // A superseded generation: the key's first attempt failed on its card, a recovery
    // allocation made a second attempt current on another card. The first card survives; only
    // the current attempt's card is raised.
    seed_card(&pool, TRACK, "card-superseded", "codex").await;
    let superseded = seed_task(
        &pool,
        TRACK,
        "recovered",
        "codex",
        TaskStatus::Failed,
        Some("card-superseded"),
    )
    .await;
    seed_op(
        &pool,
        "op-superseded",
        "codex-worker",
        &superseded,
        "KernelDispatcher",
    )
    .await;
    seed_session(
        &pool,
        TRACK,
        "session-superseded",
        "codex",
        "exited",
        "card-superseded",
        Some("op-superseded"),
    )
    .await;
    let origin = calm_types::task_recovery::TaskAttemptOrigin::Recovery {
        previous_attempt_id: superseded.clone(),
        idempotency_key: "recover-backfill".into(),
        request_fingerprint: "fixture".into(),
        reason: "fixture recovery".into(),
        actor: crate::ids::ActorId::Kernel,
        constraint: calm_types::task_recovery::TaskRecoveryConstraint::V1 {
            refs: vec![calm_types::event::TaskContextRef {
                track_id: TRACK.into(),
                block_id: "root".into(),
                rev: 1,
                hash: "a".repeat(64),
                is_root: true,
            }],
            spawn: calm_types::task_recovery::TASK_IN_TRACK_ROUTE.into(),
            declared_by: "user".into(),
        },
    };
    let origin = serde_json::to_string(&origin).unwrap();
    let current = format!("{superseded}:2");
    exec(
        &pool,
        "INSERT INTO task_attempt_allocations(attempt_id,track_id,key,generation,origin_json,\
         created_at_ms) VALUES(?1,?2,'recovered',2,?3,2)",
        &[&current, TRACK, &origin],
    )
    .await;
    seed_card(&pool, TRACK, "card-current", "codex").await;
    exec(
        &pool,
        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,depends_on_json,status,\
         worker_card_id,created_at_ms,updated_at_ms) \
         VALUES(?1,?2,'recovered','codex','g','null','[]','running','card-current',2,2)",
        &[&current, TRACK],
    )
    .await;
    seed_op(
        &pool,
        "op-current",
        "codex-worker",
        &current,
        "KernelDispatcher",
    )
    .await;
    seed_session(
        &pool,
        TRACK,
        "session-current",
        "codex",
        "running",
        "card-current",
        Some("op-current"),
    )
    .await;
    // A pre-scheduler worker: a session with no spawn op and no task row stays unbound.
    seed_card(&pool, TRACK, "card-legacy", "codex").await;
    seed_session(
        &pool,
        TRACK,
        "session-legacy",
        "codex",
        "running",
        "card-legacy",
        None,
    )
    .await;
    // A Planner-dispatched op under a task's key is not the scheduler's: no binding.
    seed_card(&pool, TRACK, "card-planner-op", "codex").await;
    let planner_op_task = seed_task(
        &pool,
        TRACK,
        "planner-op",
        "codex",
        TaskStatus::Done,
        Some("card-planner-op"),
    )
    .await;
    seed_op(
        &pool,
        "op-planner",
        "codex-worker",
        &planner_op_task,
        "AiPlanner",
    )
    .await;
    seed_session(
        &pool,
        TRACK,
        "session-planner-op",
        "codex",
        "running",
        "card-planner-op",
        Some("op-planner"),
    )
    .await;
    // A stamped attempt whose session row is gone (deleted with its card): nothing to bind.
    let deleted = seed_task(
        &pool,
        TRACK,
        "deleted-session",
        "terminal",
        TaskStatus::Done,
        Some("card-gone"),
    )
    .await;
    seed_op(
        &pool,
        "op-deleted",
        "terminal-worker",
        &deleted,
        "KernelDispatcher",
    )
    .await;
    // Occupancy: one running writer holds the checkout through its lease.
    seed_card(&pool, OCCUPANCY_TRACK, "card-writer", "codex").await;
    let writer = seed_task(
        &pool,
        OCCUPANCY_TRACK,
        "writer",
        "codex",
        TaskStatus::Running,
        Some("card-writer"),
    )
    .await;
    seed_op(
        &pool,
        "op-writer",
        "codex-worker",
        &writer,
        "KernelDispatcher",
    )
    .await;
    seed_session(
        &pool,
        OCCUPANCY_TRACK,
        "session-writer",
        "codex",
        "running",
        "card-writer",
        Some("op-writer"),
    )
    .await;
    seed_lease(
        &pool,
        OCCUPANCY_TRACK,
        "lease-writer",
        "card-writer",
        "op-writer",
        true,
    )
    .await;
    pool.close().await;

    let repo = SqlxRepo::open(&url).await.expect("real startup migrates");
    let pool = repo.pool();
    let mut conn = pool.acquire().await.unwrap();
    let column = |attempt: &str, name: &str| {
        let attempt = attempt.to_string();
        let sql = format!("SELECT {name} FROM tasks WHERE id = ?1");
        async move {
            sqlx::query_scalar::<_, Option<String>>(&sql)
                .bind(attempt)
                .fetch_one(pool)
                .await
                .unwrap()
        }
    };
    for row in &seeded {
        assert_eq!(
            column(&row.attempt, "worker_session_id").await.as_deref(),
            Some(row.session.as_str()),
            "{}: bound to the session its spawn op started",
            row.attempt
        );
        assert_eq!(
            column(&row.attempt, "worker_card_id").await.as_deref(),
            Some(row.card.as_str()),
            "{}: the card stamp, filled for a dispatched row",
            row.attempt
        );
        let view = session_binding_tx(&mut conn, &row.session)
            .await
            .unwrap()
            .expect("view row");
        assert_eq!(view.attempt_id.as_deref(), Some(row.attempt.as_str()));
        assert_eq!(view.attempt_status, Some(row.status));
        assert_eq!(view.session_active, row.session_active, "{}", row.session);
        let expected = match (row.session_active, row.status) {
            (false, _) => WorkerBinding::NoSession,
            (true, TaskStatus::Dispatched | TaskStatus::Running) => WorkerBinding::Live {
                attempt_id: row.attempt.clone(),
                session_id: row.session.clone(),
                status: row.status,
            },
            (true, _) => WorkerBinding::Parked {
                last_attempt_id: row.attempt.clone(),
                session_id: row.session.clone(),
            },
        };
        for of in [
            WorkerOf::Session(&row.session),
            WorkerOf::Attempt(&row.attempt),
        ] {
            assert_eq!(
                worker_binding_tx(&mut conn, of).await.unwrap(),
                expected,
                "{}: authority is the binding and the session's liveness",
                row.attempt
            );
        }
        if let Some(lease) = &row.lease {
            let attempt: String =
                sqlx::query_scalar("SELECT attempt_id FROM workspace_leases WHERE lease_id = ?1")
                    .bind(lease)
                    .fetch_one(pool)
                    .await
                    .unwrap();
            assert_eq!(
                attempt, row.attempt,
                "{lease} belongs to its owner op's attempt"
            );
        }
    }
    assert_eq!(
        column(&isolated, "worker_session_id").await.as_deref(),
        Some("session-isolated")
    );
    assert_eq!(
        column(&spawn_failed, "worker_session_id").await.as_deref(),
        Some("session-spawn-failed")
    );
    assert_eq!(
        column(&spawn_failed, "worker_card_id").await.as_deref(),
        Some("card-spawn-failed"),
        "the backfill stamps the unstamped spawn-failed attempt's card"
    );
    for unbound in [&planner_op_task, &deleted] {
        assert_eq!(
            column(unbound, "worker_session_id").await,
            None,
            "{unbound}"
        );
    }
    assert_eq!(
        worker_binding_tx(&mut conn, WorkerOf::Session("session-legacy"))
            .await
            .unwrap(),
        WorkerBinding::Unbound {
            session_id: "session-legacy".into()
        }
    );

    // The activity fold raises each attempt's card, ended sessions included.
    let rows = TrackRows {
        track: sql::track_row(pool, TRACK)
            .await
            .unwrap()
            .expect("track row"),
        tasks: sql::current_tasks(pool, TRACK).await.unwrap(),
        sessions: sql::eligible_sessions(pool, TRACK).await.unwrap(),
        live_harness_sessions: Vec::new(),
        output: Default::default(),
        now_ms: crate::model::now_ms(),
        notifications: sql::notification_rows(pool, TRACK).await.unwrap(),
    };
    let task_cards: BTreeMap<String, Option<String>> = rows
        .tasks
        .iter()
        .map(|task| (format!("{TRACK}:{}", task.key), task.worker_card_id.clone()))
        .collect();
    let cards: BTreeMap<String, CardState> = fold(TRACK, &rows)
        .cards
        .into_iter()
        .map(|card| (card.card_id, card.state))
        .collect();
    for row in &seeded {
        assert_eq!(
            task_cards.get(&row.attempt).cloned().flatten().as_deref(),
            Some(row.card.as_str()),
            "{}: the activity reads the attempt's card from its binding",
            row.attempt
        );
        let expected = match row.status {
            TaskStatus::Dispatched | TaskStatus::Running | TaskStatus::Verifying => {
                Some(CardState::Working)
            }
            TaskStatus::Failed => Some(CardState::Failed),
            _ => None,
        };
        if let Some(expected) = expected {
            assert!(
                cards.get(&row.card).is_some_and(|state| *state >= expected),
                "{}: card {:?}, expected at least {expected:?}",
                row.attempt,
                cards.get(&row.card)
            );
        }
    }

    assert_eq!(
        column(&superseded, "worker_session_id").await.as_deref(),
        Some("session-superseded"),
        "the superseded attempt keeps its history"
    );
    assert_eq!(
        cards.get("card-superseded"),
        None,
        "a superseded attempt's card is not raised"
    );
    assert_eq!(cards.get("card-current"), Some(&CardState::Working));

    // Checkout occupancy excludes the asking attempt's own lease, by the backfilled attempt.
    assert_eq!(
        checkout_occupancy(&mut conn, OCCUPANCY_TRACK, &writer)
            .await
            .unwrap(),
        CheckoutOccupancy::Free
    );
    assert_eq!(
        checkout_occupancy(&mut conn, OCCUPANCY_TRACK, "another-attempt")
            .await
            .unwrap(),
        CheckoutOccupancy::Busy
    );
}

/// History readers ignore the session's liveness: an attempt that failed and whose worker session
/// then ended still raises its card in the activity fold and still names its card in the run view.
#[tokio::test]
async fn ended_worker_keeps_its_activity_and_run() {
    use crate::db::prelude::*;
    use crate::session_projection_repo::WorkerSessionState;
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = repo
        .area_create(crate::model::NewArea {
            name: "ended".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(crate::model::NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "ended".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: crate::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = repo
        .card_create(crate::model::NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: serde_json::Value::Null,
        })
        .await
        .unwrap();
    let card_id = card.id.to_string();
    exec(
        repo.pool(),
        "UPDATE cards SET role = 'worker' WHERE id = ?1",
        &[card_id.as_str()],
    )
    .await;
    let attempt = format!("{}:ended", track.id);
    exec(
        repo.pool(),
        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,depends_on_json,status,\
         declared_by,created_at_ms,updated_at_ms) \
         VALUES(?1,?2,'ended','codex','g','null','[]','dispatched','user',1,1)",
        &[attempt.as_str(), track.id.as_str()],
    )
    .await;
    let session = crate::test_seams::bind_running_worker_for_test(repo.pool(), &attempt, &card_id)
        .await
        .unwrap();
    let mut tx = crate::db::sqlite::begin_immediate_tx(repo.pool())
        .await
        .unwrap();
    let flipped = crate::db::sqlite::task_fail_from_worker_tx(
        &mut tx,
        &attempt,
        track.id.as_str(),
        crate::db::sqlite::TaskReporter::Session {
            session_id: &session,
        },
        "worker-reported: gave up",
        2,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(flipped, 1);
    repo.session_projection_complete_for_card(&card_id, WorkerSessionState::Exited)
        .await
        .unwrap();
    let mut conn = repo.pool().acquire().await.unwrap();
    assert_eq!(
        worker_binding_tx(&mut conn, WorkerOf::Session(&session))
            .await
            .unwrap(),
        WorkerBinding::NoSession,
        "premise: the worker session has ended"
    );

    let pool = repo.pool();
    let rows = TrackRows {
        track: sql::track_row(pool, track.id.as_str())
            .await
            .unwrap()
            .expect("track row"),
        tasks: sql::current_tasks(pool, track.id.as_str()).await.unwrap(),
        sessions: sql::eligible_sessions(pool, track.id.as_str())
            .await
            .unwrap(),
        live_harness_sessions: Vec::new(),
        output: Default::default(),
        now_ms: crate::model::now_ms(),
        notifications: sql::notification_rows(pool, track.id.as_str())
            .await
            .unwrap(),
    };
    let fold = fold(track.id.as_str(), &rows);
    assert!(
        fold.cards
            .iter()
            .any(|c| c.card_id == card_id && c.state == CardState::Failed),
        "the failed attempt's card keeps its verdict after its session ended: {:?}",
        fold.cards
    );

    let role_cache = crate::card_role_cache::CardRoleCache::new();
    repo.seed_card_role_cache(&role_cache).await.unwrap();
    let area_cache = crate::track_area_cache::TrackAreaCache::new();
    repo.seed_track_area_cache(&area_cache).await.unwrap();
    let write = crate::state::WriteContext::new(role_cache, area_cache);
    let track_row = repo.track_get(track.id.as_str()).await.unwrap().unwrap();
    let run = crate::track_fs_view::TrackFsView::new(&repo, &write)
        .cat(&track_row, &format!("runs/{attempt}.json"))
        .await
        .expect("the run of the ended worker");
    let run: serde_json::Value = serde_json::from_str(&run.content).unwrap();
    assert_eq!(
        run["worker_card_id"].as_str(),
        Some(card_id.as_str()),
        "the run names the ended worker's card: {run}"
    );
}
