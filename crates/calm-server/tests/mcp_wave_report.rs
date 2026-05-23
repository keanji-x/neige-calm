//! Issue #229 PR B — `mcp_server::tools::wave_report` integration smoke.
//!
//! Same shape as `mcp_wave_state.rs`: in-memory `SqlxRepo`, an
//! `EventBus`, a pre-seeded `CardRoleCache`, and an `AppContext`
//! constructed directly so we can drive the three tool handlers
//! (`calm.report.read`, `calm.report.write`, `calm.report.edit`) as
//! plain async fns.
//!
//! Coverage:
//!
//!   1. `report_read` (spec) returns the initial seeded body + summary
//!      + schemaVersion + updated_at.
//!   2. `report_write` (spec) replaces the body wholesale, bumps
//!      `updated_at`, and emits one `card.updated` event.
//!   3. `report_write` keeps the existing summary when omitted; honors
//!      a non-null override when provided.
//!   4. `report_edit` happy path — unique substring replacement.
//!   5. `report_edit` rejects missing `old_string` (-32602).
//!   6. `report_edit` rejects duplicate matches without `replace_all`
//!      (-32602).
//!   7. `report_edit` honors `replace_all=true` on multi-match.
//!   8. `report_edit` short-circuits when `old_string == new_string`
//!      (no write, no event, returns current `updated_at`).
//!   9. Worker calling any of the three is refused at the soft role
//!      gate (-32602 "tool requires role=Spec got=Worker").
//!  10. Spec card on a different wave cannot reach this wave's report
//!      — the (spec_card_id → wave_id → report_card) lookup confines
//!      writes to the caller's own wave.

#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::ids::{CardId, WaveId};
use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::tools::wave_report::{
    TOOL_REPORT_EDIT, TOOL_REPORT_READ, TOOL_REPORT_WRITE,
};
use calm_server::mcp_server::{CardIdentity, ToolRegistry};
use calm_server::model::{CardRole, NewCard, NewCove, NewWave};
use calm_server::plugin_host::mcp::RpcError;
use calm_server::wave_report::WaveReportPayload;
use serde_json::{Value, json};

/// In-memory fixture: one cove → one wave → one spec card + one
/// wave-report card + one worker card. Mirrors the post-`create_wave`
/// shape (spec + wave-report kernel-owned) plus a worker for the
/// cross-role tests.
struct Boot {
    ctx: Arc<AppContext>,
    registry: Arc<ToolRegistry>,
    repo: Arc<dyn Repo>,
    wave_id: WaveId,
    spec_card_id: CardId,
    report_card_id: CardId,
    worker_card_id: CardId,
}

async fn boot() -> Boot {
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let cove = repo
        .cove_create(NewCove {
            name: "report-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "report wave".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
        })
        .await
        .unwrap();
    let spec_card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: Value::Null,
        })
        .await
        .unwrap();
    // The wave-report card row matching what `routes::waves::create_wave`
    // (and migration 0014) mint. Plain `card_create` here writes the row
    // with role=Plain and deletable=true — that's fine for these
    // integration tests because the MCP tools look up the row by
    // `kind == "wave-report"`, not by role/deletable. We pin the role
    // in the cache below to mirror production semantics.
    let report_card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "wave-report".into(),
            sort: Some(-1.0),
            payload: serde_json::to_value(WaveReportPayload::initial()).unwrap(),
        })
        .await
        .unwrap();
    let worker_card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: Value::Null,
        })
        .await
        .unwrap();

    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    card_role_cache.insert(spec_card.id.clone(), CardRole::Spec, wave.id.clone());
    card_role_cache.insert(
        report_card.id.clone(),
        CardRole::ReportCard,
        wave.id.clone(),
    );
    card_role_cache.insert(worker_card.id.clone(), CardRole::Worker, wave.id.clone());

    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();
    let ctx = Arc::new(AppContext {
        repo: route_repo,
        events,
        card_role_cache,
        event_cursor_cache: calm_server::event_cursor::EventCursorCache::new(),
        wave_cove_cache,
    });

    let mut registry = ToolRegistry::new();
    calm_server::mcp_server::tools::register_default_tools(&mut registry);
    let registry = Arc::new(registry);

    Boot {
        ctx,
        registry,
        repo,
        wave_id: wave.id,
        spec_card_id: spec_card.id,
        report_card_id: report_card.id,
        worker_card_id: worker_card.id,
    }
}

async fn call_tool(
    boot: &Boot,
    name: &str,
    identity: CardIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    let handler = boot
        .registry
        .lookup(name)
        .unwrap_or_else(|| panic!("tool not registered: {name}"));
    handler(boot.ctx.clone(), identity, args).await
}

fn spec_identity(boot: &Boot) -> CardIdentity {
    CardIdentity {
        card_id: boot.spec_card_id.clone(),
        role: CardRole::Spec,
    }
}

fn worker_identity(boot: &Boot) -> CardIdentity {
    CardIdentity {
        card_id: boot.worker_card_id.clone(),
        role: CardRole::Worker,
    }
}

/// Subscribe to the bus and collect `n` envelopes — small helper so
/// the write/edit tests can assert on the emitted `card.updated`.
async fn collect_n(events: &EventBus, n: usize) -> Vec<calm_server::event::BroadcastEnvelope> {
    let mut sub = events.subscribe();
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        match tokio::time::timeout(Duration::from_secs(2), sub.recv()).await {
            Ok(Ok(env)) => out.push(env),
            Ok(Err(_lag)) => break,
            Err(_timeout) => break,
        }
    }
    out
}

// ---------------------------------------------------------------------------
// calm.report.read
// ---------------------------------------------------------------------------

#[tokio::test]
async fn read_returns_initial_seeded_body() {
    let boot = boot().await;
    let out = call_tool(&boot, TOOL_REPORT_READ, spec_identity(&boot), json!({}))
        .await
        .expect("spec can read the report");
    assert_eq!(
        out.get("body").and_then(Value::as_str),
        Some("# Goal\n\n_The spec agent will fill this in._\n")
    );
    assert_eq!(out.get("summary").and_then(Value::as_str), Some(""));
    assert_eq!(out.get("schemaVersion").and_then(Value::as_u64), Some(1));
    assert!(
        out.get("updated_at").and_then(Value::as_i64).unwrap_or(0) > 0,
        "updated_at is a positive timestamp; got {out:?}",
    );
}

#[tokio::test]
async fn read_refuses_worker() {
    let boot = boot().await;
    let err = call_tool(&boot, TOOL_REPORT_READ, worker_identity(&boot), json!({}))
        .await
        .expect_err("worker must be denied");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(err.message.contains("Spec"), "msg = {err:?}");
}

// ---------------------------------------------------------------------------
// calm.report.write
// ---------------------------------------------------------------------------

#[tokio::test]
async fn write_replaces_body_and_emits_card_updated() {
    let boot = boot().await;
    let events = boot.ctx.events.clone();
    let report_id = boot.report_card_id.clone();
    let sub = tokio::spawn(async move { collect_n(&events, 1).await });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let out = call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({
            "body": "# Goal\n\nrefactored everything\n",
            "summary": "done refactoring"
        }),
    )
    .await
    .expect("spec writes successfully");
    let new_updated_at = out
        .get("updated_at")
        .and_then(Value::as_i64)
        .expect("updated_at i64");

    // Bus saw a card.updated envelope for the report card with the new body.
    let envs = sub.await.expect("collector ok");
    assert_eq!(envs.len(), 1, "exactly one envelope emitted; got {envs:?}");
    match &envs[0].event {
        Event::CardUpdated(c) => {
            assert_eq!(c.id, report_id, "envelope is for the report card");
            assert_eq!(c.kind, "wave-report");
            let payload: WaveReportPayload =
                serde_json::from_value(c.payload.clone()).expect("payload deserializes");
            assert_eq!(payload.body, "# Goal\n\nrefactored everything\n");
            assert_eq!(payload.summary, "done refactoring");
            assert_eq!(payload.schema_version, 1);
            assert_eq!(c.updated_at, new_updated_at);
        }
        other => panic!("expected CardUpdated, got {other:?}"),
    }
    assert!(matches!(envs[0].scope, EventScope::Card { .. }));

    // DB also has the new shape.
    let card = boot
        .repo
        .card_get(report_id.as_str())
        .await
        .unwrap()
        .expect("report card row");
    let payload: WaveReportPayload =
        serde_json::from_value(card.payload).expect("payload deserializes");
    assert_eq!(payload.body, "# Goal\n\nrefactored everything\n");
}

#[tokio::test]
async fn write_preserves_summary_when_omitted() {
    let boot = boot().await;
    // First write sets a known summary.
    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({ "body": "a", "summary": "preserved" }),
    )
    .await
    .unwrap();
    // Second write omits summary; it should keep "preserved".
    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({ "body": "b" }),
    )
    .await
    .unwrap();

    let card = boot
        .repo
        .card_get(boot.report_card_id.as_str())
        .await
        .unwrap()
        .unwrap();
    let payload: WaveReportPayload = serde_json::from_value(card.payload).unwrap();
    assert_eq!(payload.body, "b");
    assert_eq!(payload.summary, "preserved");
}

#[tokio::test]
async fn write_refuses_worker() {
    let boot = boot().await;
    let err = call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        worker_identity(&boot),
        json!({ "body": "evil" }),
    )
    .await
    .expect_err("worker must be denied");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
}

#[tokio::test]
async fn write_rejects_missing_body() {
    let boot = boot().await;
    let err = call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({ "summary": "no body" }),
    )
    .await
    .expect_err("missing body must be rejected");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(err.message.contains("body"), "msg = {err:?}");
}

// ---------------------------------------------------------------------------
// calm.report.edit
// ---------------------------------------------------------------------------

#[tokio::test]
async fn edit_unique_substring_replacement_happy_path() {
    let boot = boot().await;
    // Seed a body with a known unique substring.
    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({ "body": "# Goal\n\nuntouched marker XYZ here\n" }),
    )
    .await
    .unwrap();
    // Now edit it.
    let out = call_tool(
        &boot,
        TOOL_REPORT_EDIT,
        spec_identity(&boot),
        json!({ "old_string": "XYZ", "new_string": "ABC" }),
    )
    .await
    .expect("happy edit");
    assert!(out.get("updated_at").and_then(Value::as_i64).is_some());

    let card = boot
        .repo
        .card_get(boot.report_card_id.as_str())
        .await
        .unwrap()
        .unwrap();
    let payload: WaveReportPayload = serde_json::from_value(card.payload).unwrap();
    assert_eq!(payload.body, "# Goal\n\nuntouched marker ABC here\n");
}

#[tokio::test]
async fn edit_rejects_old_string_not_found() {
    let boot = boot().await;
    let err = call_tool(
        &boot,
        TOOL_REPORT_EDIT,
        spec_identity(&boot),
        json!({ "old_string": "nowhere-in-body", "new_string": "x" }),
    )
    .await
    .expect_err("missing old_string must error");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(err.message.contains("not found"), "msg = {err:?}");
}

#[tokio::test]
async fn edit_rejects_duplicate_without_replace_all() {
    let boot = boot().await;
    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({ "body": "TODO foo\nTODO bar\n" }),
    )
    .await
    .unwrap();
    let err = call_tool(
        &boot,
        TOOL_REPORT_EDIT,
        spec_identity(&boot),
        json!({ "old_string": "TODO", "new_string": "DONE" }),
    )
    .await
    .expect_err("duplicate without replace_all must error");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(err.message.contains("not unique"), "msg = {err:?}");
    assert!(err.message.contains("replace_all"), "msg = {err:?}");
}

#[tokio::test]
async fn edit_replace_all_on_duplicates() {
    let boot = boot().await;
    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({ "body": "TODO foo\nTODO bar\nTODO baz\n" }),
    )
    .await
    .unwrap();
    call_tool(
        &boot,
        TOOL_REPORT_EDIT,
        spec_identity(&boot),
        json!({
            "old_string": "TODO",
            "new_string": "DONE",
            "replace_all": true
        }),
    )
    .await
    .expect("replace_all=true succeeds");

    let card = boot
        .repo
        .card_get(boot.report_card_id.as_str())
        .await
        .unwrap()
        .unwrap();
    let payload: WaveReportPayload = serde_json::from_value(card.payload).unwrap();
    assert_eq!(payload.body, "DONE foo\nDONE bar\nDONE baz\n");
}

#[tokio::test]
async fn edit_noop_when_old_equals_new() {
    let boot = boot().await;
    // Seed and capture the row's updated_at *after* the seed write so
    // we have a stable baseline.
    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({ "body": "stable\n" }),
    )
    .await
    .unwrap();
    let before = boot
        .repo
        .card_get(boot.report_card_id.as_str())
        .await
        .unwrap()
        .unwrap();
    let before_ts = before.updated_at;

    // Subscribe — if a no-op accidentally emits, the collector picks
    // it up so the test fails.
    let events = boot.ctx.events.clone();
    let sub = tokio::spawn(async move { collect_n(&events, 1).await });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let out = call_tool(
        &boot,
        TOOL_REPORT_EDIT,
        spec_identity(&boot),
        json!({ "old_string": "stable", "new_string": "stable" }),
    )
    .await
    .expect("no-op edit succeeds");
    assert_eq!(
        out.get("updated_at").and_then(Value::as_i64),
        Some(before_ts),
        "no-op returns the unchanged updated_at",
    );
    // Bus should be empty (timeout returns 0 envelopes).
    let envs = sub.await.expect("collector ok");
    assert!(
        envs.is_empty(),
        "no-op must not emit; got {} envelopes",
        envs.len()
    );

    // Row's updated_at unchanged.
    let after = boot
        .repo
        .card_get(boot.report_card_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.updated_at, before_ts);
}

#[tokio::test]
async fn edit_refuses_worker() {
    let boot = boot().await;
    let err = call_tool(
        &boot,
        TOOL_REPORT_EDIT,
        worker_identity(&boot),
        json!({ "old_string": "Goal", "new_string": "Pwn" }),
    )
    .await
    .expect_err("worker must be denied");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
}

// ---------------------------------------------------------------------------
// Cross-wave isolation: a spec card on wave A cannot reach wave B's report.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn spec_from_different_wave_cannot_reach_this_wave_report() {
    let boot = boot().await;
    // Mint a second wave + a second spec card, and use that spec
    // identity to call `report.write`. The tool resolves the report
    // through (spec_card_id → spec_card.wave_id → wave's report card),
    // so the write lands on wave 2's report — *not* wave 1's. We
    // confirm wave 1's body is untouched.

    let cove2 = boot
        .repo
        .cove_create(NewCove {
            name: "wave-b".into(),
            color: "#0f0".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave2 = boot
        .repo
        .wave_create(NewWave {
            cove_id: cove2.id,
            title: "wave 2".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
        })
        .await
        .unwrap();
    let spec2 = boot
        .repo
        .card_create(NewCard {
            wave_id: wave2.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: Value::Null,
        })
        .await
        .unwrap();
    let report2 = boot
        .repo
        .card_create(NewCard {
            wave_id: wave2.id.clone(),
            kind: "wave-report".into(),
            sort: Some(-1.0),
            payload: serde_json::to_value(WaveReportPayload::initial()).unwrap(),
        })
        .await
        .unwrap();
    boot.ctx
        .card_role_cache
        .insert(spec2.id.clone(), CardRole::Spec, wave2.id.clone());

    // Call from spec2's identity.
    let spec2_identity = CardIdentity {
        card_id: spec2.id,
        role: CardRole::Spec,
    };
    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec2_identity,
        json!({ "body": "wave 2 only\n", "summary": "wave 2" }),
    )
    .await
    .expect("spec2 writes its own wave's report");

    // Wave 1's report is untouched.
    let card1 = boot
        .repo
        .card_get(boot.report_card_id.as_str())
        .await
        .unwrap()
        .unwrap();
    let payload1: WaveReportPayload = serde_json::from_value(card1.payload).unwrap();
    assert_eq!(
        payload1.body, "# Goal\n\n_The spec agent will fill this in._\n",
        "wave 1's report is the original seed body — cross-wave isolation held",
    );

    // Wave 2's report has the new body.
    let card2 = boot
        .repo
        .card_get(report2.id.as_str())
        .await
        .unwrap()
        .unwrap();
    let payload2: WaveReportPayload = serde_json::from_value(card2.payload).unwrap();
    assert_eq!(payload2.body, "wave 2 only\n");
    assert_eq!(payload2.summary, "wave 2");

    // Use wave_id to silence unused-variable lints — referenced for
    // potential future per-wave-id assertions.
    let _ = boot.wave_id.clone();
}
