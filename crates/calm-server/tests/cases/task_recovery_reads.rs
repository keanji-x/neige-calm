//! Public recovery reads retain logical identity and current blockers.
use crate::mcp_track_report::{boot, call_tool, planner_identity};
use crate::task_recovery::{
    current, declaration, declare, finish, ordinary_codex_declaration, recovery_args,
    time_out_claimed_worker_holding_lease,
};
use calm_server::ids::ActorId;
use calm_server::model::{NewCard, NewTrack};
use calm_server::task_recovery::task_recovery_view;
use calm_server::track_report::{TrackReportPayload, persist_report, resolve_report_for_track};
use serde_json::{Value, json};

pub(super) async fn rest_attempts(
    boot: &crate::mcp_track_report::Boot,
    key: &str,
    expected: axum::http::StatusCode,
) -> Value {
    rest_attempts_for_track(boot, boot.track_id.as_str(), key, expected).await
}

async fn rest_attempts_for_track(
    boot: &crate::mcp_track_report::Boot,
    track_id: &str,
    key: &str,
    expected: axum::http::StatusCode,
) -> Value {
    use axum::{Extension, body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let response = calm_server::routes::task_recovery::router()
        .with_state(crate::task_projection_acceptance::route_state(boot).await)
        .layer(Extension(crate::task_projection_acceptance::principal()))
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .oneshot(
            Request::builder()
                .uri(format!("/api/tracks/{track_id}/tasks/{key}/attempts"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(status, expected, "{}", String::from_utf8_lossy(&bytes));
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn task_recovery_history_is_empty_before_initial_allocation() {
    let boot = boot().await;
    let mut payload = declaration("waiting", &[]);
    payload["ready"] = json!(false);
    let (block, revision) = declare(&boot, payload).await;
    let pool = boot.repo.sqlite_pool().unwrap();
    assert!(
        calm_server::db::sqlite::task_attempt_current_pool(
            &pool,
            boot.track_id.as_str(),
            "waiting"
        )
        .await
        .unwrap()
        .is_none()
    );
    let view = rest_attempts(&boot, "waiting", axum::http::StatusCode::OK).await;
    assert!(view.as_object().unwrap().contains_key("current"));
    assert_eq!(view["current"], Value::Null);
    assert_eq!(view["attempts"], json!([]));
    assert_eq!(view["recovery"]["allowed"], false);
    assert_eq!(view["recovery"]["code"], "not_started");
    rest_attempts(&boot, "unknown", axum::http::StatusCode::NOT_FOUND).await;
    rest_attempts_for_track(
        &boot,
        "unknown-track",
        "waiting",
        axum::http::StatusCode::NOT_FOUND,
    )
    .await;
    call_tool(
        &boot,
        calm_server::mcp_server::tools::track_report_blocks::TOOL_REPORT_BLOCKS_UPSERT,
        planner_identity(&boot),
        json!({"id":block,"kind":"task","payload":declaration("waiting", &[]),"if_rev":revision}),
    )
    .await
    .unwrap();
    let allocated = current(&boot, "waiting").await;
    let view = rest_attempts(&boot, "waiting", axum::http::StatusCode::OK).await;
    assert_eq!(view["current"]["attempt_id"], allocated.id);
    assert_eq!(view["current"]["generation"], 1);
    assert_eq!(view["attempts"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn task_recovery_list_keeps_absent_projection_with_ready_blocker() {
    let boot = boot().await;
    let (block, revision) = declare(&boot, declaration("b", &[])).await;
    let b = current(&boot, "b").await;
    finish(&boot, &b, false).await;
    let receipt = call_tool(
        &boot,
        "calm.plan.recover",
        planner_identity(&boot),
        recovery_args(&b, "read-b"),
    )
    .await
    .unwrap();
    let mut withdrawn = declaration("b", &[]);
    withdrawn["ready"] = json!(false);
    call_tool(
        &boot,
        "calm.report.blocks.upsert",
        planner_identity(&boot),
        json!({"id":block,"kind":"task","payload":withdrawn,"if_rev":revision}),
    )
    .await
    .unwrap();
    let list = call_tool(&boot, "calm.plan.list", planner_identity(&boot), json!({}))
        .await
        .unwrap();
    let entry = list["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["key"] == "b")
        .expect("current allocation must remain visible without a pending row");
    assert_eq!(entry["attempt_id"], receipt["attempt_id"]);
    assert_eq!(entry["status"], "awaiting_projection");
    assert!(entry["blocking_reason"].as_str().unwrap().contains("ready"));
    let summary = call_tool(
        &boot,
        "calm.plan.list",
        planner_identity(&boot),
        json!({"detail":"summary","key":"b"}),
    )
    .await
    .unwrap();
    let compact = &summary["tasks"][0];
    for field in [
        "attempt_id",
        "generation",
        "status",
        "blocking_reason",
        "recovery",
    ] {
        assert_eq!(compact[field], entry[field]);
    }
    assert_eq!(compact["task_projection"], "unavailable");
    let omissions = compact["omitted_fields"].as_array().unwrap();
    for path in omissions {
        assert!(
            entry.pointer(path.as_str().unwrap()).is_some(),
            "invented omission {path}: {entry}"
        );
    }
    for field in entry.as_object().unwrap().keys() {
        if compact.get(field).is_none() {
            assert!(
                omissions.contains(&json!(format!("/{field}"))),
                "missing omission for {field}"
            );
        }
    }

    let view = serde_json::to_value(
        task_recovery_view(
            boot.repo.as_ref(),
            boot.track_id.as_str(),
            "b",
            ActorId::User,
            calm_server::scheduler::DEFAULT_TRACK_TASK_BUDGET,
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(view["current"]["blocking_reason"], entry["blocking_reason"]);
    assert!(view["attempts"][0]["blocking_reason"].is_null());
}

#[tokio::test]
async fn task_recovery_history_retains_current_dependency_blocker() {
    let boot = boot().await;
    declare(&boot, declaration("b", &[])).await;
    declare(&boot, declaration("c", &["b"])).await;
    let b = current(&boot, "b").await;
    finish(&boot, &b, false).await;
    let report = call_tool(
        &boot,
        "calm.report.read",
        planner_identity(&boot),
        json!({}),
    )
    .await
    .unwrap();
    let reason = report["taskDiagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|value| value["key"] == "c")
        .unwrap()["pendingReason"]["message"]
        .clone();
    assert!(reason.as_str().unwrap().contains("`b`"));
    let view = serde_json::to_value(
        task_recovery_view(
            boot.repo.as_ref(),
            boot.track_id.as_str(),
            "c",
            ActorId::User,
            calm_server::scheduler::DEFAULT_TRACK_TASK_BUDGET,
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(view["current"]["blocking_reason"], reason);
    let rest = rest_attempts(&boot, "c", axum::http::StatusCode::OK).await;
    assert_eq!(rest["current"]["blocking_reason"], reason);
}

#[tokio::test]
async fn task_recovery_deleted_frozen_reference_denies_only_affected_capability() {
    let boot = boot().await;
    let target = boot
        .repo
        .track_create(NewTrack {
            template_input: None,
            area_id: boot.area_id.clone(),
            title: "reference source".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    boot.repo
        .card_create(NewCard {
            track_id: target.id.clone(),
            title: None,
            kind: "track-report".into(),
            sort: None,
            payload: serde_json::to_value(TrackReportPayload::initial()).unwrap(),
        })
        .await
        .unwrap();
    let (track, card, payload) = resolve_report_for_track(boot.repo.as_ref(), target.id.as_str())
        .await
        .unwrap();
    let revision = payload.doc_rev;
    let written = persist_report(
        boot.repo.as_ref(),
        &boot.ctx.events,
        &boot.ctx.write,
        ActorId::User,
        calm_server::event::EditAuthor::User,
        track,
        card,
        payload,
        TrackReportPayload::new("", "## Input\nFrozen input"),
        revision,
        None,
        None,
        false,
    )
    .await
    .unwrap();
    let block = written.payload["blocks"][0]["id"].as_str().unwrap();
    let mut b_decl = declaration("b", &[]);
    b_decl["refs"] = json!([calm_types::report_links::format_track_destination(
        target.id.as_str(),
        Some(block)
    )]);
    declare(&boot, declaration("a", &[])).await;
    declare(&boot, b_decl).await;
    let b = current(&boot, "b").await;
    finish(&boot, &b, false).await;
    boot.repo.track_delete(target.id.as_str()).await.unwrap();
    let view = task_recovery_view(
        boot.repo.as_ref(),
        boot.track_id.as_str(),
        "b",
        ActorId::User,
        calm_server::scheduler::DEFAULT_TRACK_TASK_BUDGET,
    )
    .await
    .expect("missing frozen input is a task capability, not a missing requested task");
    assert!(!view.recovery.allowed);
    assert!(view.recovery.reason.contains("missing"));
    assert_eq!(view.attempts.len(), 1);
    let rest = rest_attempts(&boot, "b", axum::http::StatusCode::OK).await;
    assert_eq!(rest["current"]["attempt_id"], b.id);
    assert_eq!(rest["recovery"]["allowed"], false);
    rest_attempts(&boot, "absent", axum::http::StatusCode::NOT_FOUND).await;
    let list = call_tool(&boot, "calm.plan.list", planner_identity(&boot), json!({}))
        .await
        .unwrap();
    assert_eq!(list["tasks"].as_array().unwrap().len(), 2);
    let missing = task_recovery_view(
        boot.repo.as_ref(),
        boot.track_id.as_str(),
        "absent",
        ActorId::User,
        calm_server::scheduler::DEFAULT_TRACK_TASK_BUDGET,
    )
    .await
    .unwrap_err();
    assert!(matches!(
        missing,
        calm_server::error::CalmError::NotFound(_)
    ));
}

#[tokio::test]
async fn task_recovery_plan_inventory_pages_all_current_allocations() {
    let boot = boot().await;
    let pool = boot.repo.sqlite_pool().unwrap();
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(&pool)
        .await
        .unwrap();
    for index in 0..130 {
        let key = format!("k{index:03}");
        sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,created_at_ms,updated_at_ms,finished_at_ms) VALUES(?1,?2,?3,'terminal','true','{}','done',1,1,1)")
            .bind(format!("{}:{key}", boot.track_id)).bind(boot.track_id.as_str()).bind(key).execute(&mut *tx).await.unwrap();
    }
    tx.commit().await.unwrap();
    let list = call_tool(&boot, "calm.plan.list", planner_identity(&boot), json!({}))
        .await
        .unwrap();
    let keys: Vec<_> = list["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|task| task["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys.len(), 130);
    assert_eq!(keys.first(), Some(&"k000"));
    assert_eq!(keys.last(), Some(&"k129"));
    let selected = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        call_tool(
            &boot,
            "calm.plan.list",
            planner_identity(&boot),
            json!({"detail":"summary","key":"k129"}),
        ),
    )
    .await
    .expect("direct lookup must terminate")
    .unwrap();
    assert_eq!(selected["tasks"].as_array().unwrap().len(), 1);
    assert_eq!(selected["tasks"][0]["key"], "k129");
    let missing = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        call_tool(
            &boot,
            "calm.plan.list",
            planner_identity(&boot),
            json!({"detail":"summary","key":"unknown"}),
        ),
    )
    .await
    .expect("missing lookup must terminate")
    .unwrap_err();
    assert!(missing.message.contains("current execution unavailable"));

    assert_eq!(
        keys.iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        130
    );
}

#[tokio::test]
async fn task_recovery_blocker_uses_live_configured_budget_like_report_read() {
    let boot = boot().await;
    declare(&boot, declaration("a", &[])).await;
    declare(&boot, declaration("b", &[])).await;
    let pool = boot.repo.sqlite_pool().unwrap();
    sqlx::query("UPDATE tasks SET status='running' WHERE track_id=?1 AND key='a'")
        .bind(boot.track_id.as_str())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO settings(key,value,updated_at) VALUES(?1,'1',1) ON CONFLICT(key) DO UPDATE SET value='1',updated_at=excluded.updated_at",
    )
    .bind(calm_server::routes::settings::TASK_BUDGET_DEFAULT_KEY)
    .execute(&pool)
    .await
    .unwrap();
    let view = task_recovery_view(
        boot.repo.as_ref(),
        boot.track_id.as_str(),
        "b",
        ActorId::User,
        6,
    )
    .await
    .unwrap();
    let report = call_tool(
        &boot,
        "calm.report.read",
        planner_identity(&boot),
        json!({}),
    )
    .await
    .unwrap();
    let reason = report["taskDiagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["key"] == "b")
        .unwrap()["pendingReason"]["message"]
        .clone();
    assert_eq!(reason, "Queued 1/1");
    assert_eq!(
        serde_json::to_value(view).unwrap()["current"]["blocking_reason"],
        reason
    );
}

async fn list_entry(boot: &crate::mcp_track_report::Boot, args: Value) -> Value {
    let list = call_tool(boot, "calm.plan.list", planner_identity(boot), args)
        .await
        .unwrap();
    list["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["key"] == "b")
        .cloned()
        .expect("task b is listed")
}

#[tokio::test]
async fn task_recovery_list_carries_the_worker_worktree_facts() {
    let boot = boot().await;
    declare(&boot, declaration("b", &[])).await;
    let b = current(&boot, "b").await;
    let pool = boot.repo.sqlite_pool().unwrap();
    sqlx::query("UPDATE tasks SET worker_card_id=?1 WHERE id=?2")
        .bind(boot.worker_card_id.as_str())
        .bind(&b.id)
        .execute(&pool)
        .await
        .unwrap();
    let track = boot.track_id.as_str().to_string();
    let card = boot.worker_card_id.as_str().to_string();

    // No lease row → no `worktree` key (not a null, not an empty object).
    let entry = list_entry(&boot, json!({})).await;
    assert!(
        entry.get("worktree").is_none(),
        "no lease must render no worktree key: {entry}"
    );

    // A lease at the canonical path, through the production acquisition; no commit yet.
    let repo_root = tempfile::tempdir().expect("tempdir");
    let lease_path = repo_root
        .path()
        .join(".claude")
        .join("worktrees")
        .join(&track)
        .join(&card);
    calm_server::test_seams::acquire_workspace_lease_for_test(
        &pool,
        &card,
        &track,
        "test-owner",
        &lease_path,
    )
    .await
    .unwrap();
    let entry = list_entry(&boot, json!({})).await;
    assert_eq!(
        entry["worktree"],
        json!({
            "path": lease_path.to_string_lossy(),
            "state": "held",
            "branch": format!("neige/{track}/{card}"),
            "removed": false,
        }),
        "lease without a commit: branch from the lease naming, no last_commit: {entry}"
    );

    // The kernel's auto commit lands a `worktree.committed` event scoped to the worker card.
    let sha = "0123456789abcdef0123456789abcdef01234567".to_string();
    let event_branch = format!("neige/{track}/{card}-from-event");
    let event = calm_server::event::Event::WorktreeCommitted {
        track_id: boot.track_id.clone(),
        card_id: boot.worker_card_id.clone(),
        commit_sha: sha.clone(),
        branch: event_branch.clone(),
    };
    let scope = calm_server::event::EventScope::Card {
        card: boot.worker_card_id.clone(),
        track: boot.track_id.clone(),
        area: boot.area_id.clone(),
    };
    calm_server::db::write_with_actor_events_typed(
        boot.repo.as_ref(),
        None,
        &boot.ctx.events,
        &boot.ctx.write,
        move |_| Box::pin(async move { Ok(((), vec![(ActorId::KernelDispatcher, scope, event)])) }),
    )
    .await
    .unwrap();
    let entry = list_entry(&boot, json!({})).await;
    assert_eq!(
        entry["worktree"],
        json!({
            "path": lease_path.to_string_lossy(),
            "state": "held",
            "branch": event_branch,
            "last_commit": sha,
            "removed": false,
        }),
        "{entry}"
    );

    // The compact projection keeps every worktree fact.
    let compact = list_entry(&boot, json!({"detail":"summary","key":"b"})).await;
    assert_eq!(compact["worktree"], entry["worktree"], "{compact}");
    assert!(
        !compact["omitted_fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path.as_str().unwrap().starts_with("/worktree")),
        "{compact}"
    );
}

/// #1727 S4 slice 1 (review 3, A3-m4) — `worktree.base_sha` and
/// `recovery.guidance.retained.base_sha` come from the lease row's
/// `base_sha`, which only a lease taken through the production
/// `acquire_workspace_lease_tx` carries (the plain lease every other fixture
/// takes writes the legacy all-NULL tuple, so those fixtures never see the
/// key — and the assertions on them stay as they are). One base-recording
/// lease: `plan.list` names the sha under `worktree` in full and summary
/// detail; once the attempt has timed out, `retained` names the same sha.
#[tokio::test]
async fn task_recovery_list_names_the_worktree_base_sha_from_the_lease_row() {
    let boot = boot().await;
    declare(&boot, ordinary_codex_declaration("b")).await;
    let pool = boot.repo.sqlite_pool().unwrap();
    let track = boot.track_id.as_str().to_string();
    let card = boot.worker_card_id.as_str().to_string();
    sqlx::query("UPDATE tasks SET worker_card_id=?1 WHERE track_id=?2 AND key='b'")
        .bind(&card)
        .bind(&track)
        .execute(&pool)
        .await
        .unwrap();
    let base_sha = "89abcdef0123456789abcdef0123456789abcdef";
    let repo_root = tempfile::tempdir().expect("tempdir");
    let lease_path = calm_server::test_seams::acquire_based_workspace_lease_for_test(
        &pool,
        &card,
        &track,
        "test-owner",
        repo_root.path(),
        base_sha,
    )
    .await
    .unwrap();
    let lease_id: String =
        sqlx::query_scalar("SELECT lease_id FROM workspace_leases WHERE card_id = ?1")
            .bind(&card)
            .fetch_one(&pool)
            .await
            .unwrap();

    let entry = list_entry(&boot, json!({})).await;
    assert_eq!(
        entry["worktree"],
        json!({
            "path": lease_path.to_string_lossy(),
            "state": "held",
            "branch": format!("neige/{track}/{card}"),
            "base_sha": base_sha,
            "removed": false,
        }),
        "full detail names the recorded base: {entry}"
    );
    let compact = list_entry(&boot, json!({"detail":"summary","key":"b"})).await;
    assert_eq!(
        compact["worktree"]["base_sha"], base_sha,
        "summary detail keeps worktree/base_sha: {compact}"
    );
    assert_eq!(compact["worktree"], entry["worktree"], "{compact}");

    // The attempt times out (the liveness timeout releases the row, the
    // directory stays): a refused recovery now carries `retained`, and the
    // base is one of the facts it retains.
    time_out_claimed_worker_holding_lease(&boot, "b", &lease_id).await;
    for args in [json!({}), json!({"detail":"summary","key":"b"})] {
        let entry = list_entry(&boot, args.clone()).await;
        assert_eq!(entry["recovery"]["allowed"], false, "{args}: {entry}");
        let retained = &entry["recovery"]["guidance"]["retained"];
        assert_eq!(
            retained["base_sha"], base_sha,
            "{args}: retained names the recorded base: {retained}"
        );
        assert_eq!(
            retained["workspace_path"],
            json!(lease_path.to_string_lossy())
        );
        assert_eq!(
            entry["worktree"]["base_sha"], base_sha,
            "{args}: the released lease still names its base: {entry}"
        );
    }
}

/// Append one card-scoped kernel event for the worker card and return its event id.
async fn append_worker_card_event(
    boot: &crate::mcp_track_report::Boot,
    event: calm_server::event::Event,
) -> i64 {
    let scope = calm_server::event::EventScope::Card {
        card: boot.worker_card_id.clone(),
        track: boot.track_id.clone(),
        area: boot.area_id.clone(),
    };
    let (_, ids) = calm_server::db::write_with_actor_events_typed(
        boot.repo.as_ref(),
        None,
        &boot.ctx.events,
        &boot.ctx.write,
        move |_| Box::pin(async move { Ok(((), vec![(ActorId::KernelDispatcher, scope, event)])) }),
    )
    .await
    .unwrap();
    ids[0]
}

/// `release_workspace_lease_for_card_*` only flips the row; `release_workspace_lease_by_id`
/// removes the worktree and appends `worktree.removed`.
#[tokio::test]
async fn task_recovery_list_worktree_facts_tell_a_removed_worktree_from_a_retained_one() {
    let boot = boot().await;
    declare(&boot, declaration("b", &[])).await;
    let b = current(&boot, "b").await;
    let pool = boot.repo.sqlite_pool().unwrap();
    sqlx::query("UPDATE tasks SET worker_card_id=?1 WHERE id=?2")
        .bind(boot.worker_card_id.as_str())
        .bind(&b.id)
        .execute(&pool)
        .await
        .unwrap();
    let track = boot.track_id.as_str().to_string();
    let card = boot.worker_card_id.as_str().to_string();
    let repo_root = tempfile::tempdir().expect("tempdir");
    let lease_path = repo_root
        .path()
        .join(".claude")
        .join("worktrees")
        .join(&track)
        .join(&card);
    let lease_path_json = json!(lease_path.to_string_lossy());
    let naming_branch = format!("neige/{track}/{card}");

    // Lease 1: acquired, then released through the flip-only production path.
    calm_server::test_seams::acquire_workspace_lease_for_test(
        &pool,
        &card,
        &track,
        "test-owner",
        &lease_path,
    )
    .await
    .unwrap();
    assert!(
        calm_server::test_seams::release_workspace_lease_for_card_for_test(
            boot.repo.as_ref(),
            &boot.ctx.events,
            &card,
        )
        .await
        .unwrap(),
        "lease 1 releases"
    );
    assert!(lease_path.is_dir(), "flip-only release keeps the checkout");
    let entry = list_entry(&boot, json!({})).await;
    assert_eq!(
        entry["worktree"],
        json!({
            "path": lease_path_json,
            "state": "released",
            "branch": naming_branch,
            "removed": false,
        }),
        "released + retained: path and branch stay, removed:false: {entry}"
    );

    // Lease 2 on the same card and path. `acquire` stamps `created_at_ms` with the wall clock and
    // ties break on a random uuid, so push lease 1 back by a tick to make "latest lease" deterministic.
    sqlx::query(
        "UPDATE workspace_leases SET created_at_ms = created_at_ms - 10 WHERE card_id = ?1",
    )
    .bind(&card)
    .execute(&pool)
    .await
    .unwrap();
    calm_server::test_seams::acquire_workspace_lease_for_test(
        &pool,
        &card,
        &track,
        "test-owner",
        &lease_path,
    )
    .await
    .unwrap();
    let lease_2: String = sqlx::query_scalar(
        "SELECT lease_id FROM workspace_leases WHERE card_id = ?1 AND state = 'held'",
    )
    .bind(&card)
    .fetch_one(&pool)
    .await
    .unwrap();
    // The kernel committed on this worktree before it went away.
    let sha = "0123456789abcdef0123456789abcdef01234567".to_string();
    append_worker_card_event(
        &boot,
        calm_server::event::Event::WorktreeCommitted {
            track_id: boot.track_id.clone(),
            card_id: boot.worker_card_id.clone(),
            commit_sha: sha.clone(),
            branch: naming_branch.clone(),
        },
    )
    .await;
    let entry = list_entry(&boot, json!({})).await;
    assert_eq!(
        entry["worktree"],
        json!({
            "path": lease_path_json,
            "state": "held",
            "branch": naming_branch,
            "last_commit": sha,
            "removed": false,
        }),
        "lease 2 held with a commit: {entry}"
    );

    // Lease 2 released through the removing production path.
    assert!(
        calm_server::test_seams::release_workspace_lease_by_id_for_test(
            &pool,
            &boot.ctx.events,
            &lease_2,
        )
        .await
        .unwrap(),
        "lease 2 releases"
    );
    assert!(
        !lease_path.exists(),
        "removing release deletes the checkout"
    );
    let removed_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM events WHERE scope_card = ?1 AND kind = 'worktree.removed'",
    )
    .bind(&card)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        removed_count, 1,
        "the removing release appends worktree.removed"
    );
    let entry = list_entry(&boot, json!({})).await;
    assert_eq!(
        entry["worktree"],
        json!({
            "state": "released",
            "last_commit": sha,
            "removed": true,
        }),
        "released + removed: no path, no branch, sha kept, removed:true: {entry}"
    );
    // `detail:"summary"` carries `removed` too.
    let compact = list_entry(&boot, json!({"detail":"summary","key":"b"})).await;
    assert_eq!(compact["worktree"], entry["worktree"], "{compact}");

    // Re-provisioned after the removal: the path is back.
    append_worker_card_event(
        &boot,
        calm_server::event::Event::WorktreeProvisioned {
            track_id: boot.track_id.clone(),
            card_id: boot.worker_card_id.clone(),
            path: lease_path.to_string_lossy().to_string(),
        },
    )
    .await;
    let entry = list_entry(&boot, json!({})).await;
    assert_eq!(
        entry["worktree"],
        json!({
            "path": lease_path_json,
            "state": "released",
            "branch": naming_branch,
            "last_commit": sha,
            "removed": false,
        }),
        "provisioned after removed: path and branch are back: {entry}"
    );
}
