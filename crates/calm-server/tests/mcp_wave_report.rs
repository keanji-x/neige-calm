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
use calm_server::event::{EditAuthor, Event, EventBus, EventScope};
use calm_server::ids::{CardId, WaveId};
use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::tools::wave_report::{
    TOOL_REPORT_EDIT, TOOL_REPORT_READ, TOOL_REPORT_WRITE,
};
use calm_server::mcp_server::{ToolCallIdentity, ToolRegistry};
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
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
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
        wave_cove_cache,
        daemon_token_hash: None,
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
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    let handler = boot
        .registry
        .lookup(name)
        .unwrap_or_else(|| panic!("tool not registered: {name}"));
    handler(boot.ctx.clone(), identity, args).await
}

fn spec_identity(boot: &Boot) -> ToolCallIdentity {
    ToolCallIdentity {
        card_id: boot.spec_card_id.as_str().to_string(),
        role: CardRole::Spec,
        wave_id: Some(boot.wave_id.as_str().to_string()),
        thread_id: "spec-thread".to_string(),
    }
}

fn worker_identity(boot: &Boot) -> ToolCallIdentity {
    ToolCallIdentity {
        card_id: boot.worker_card_id.as_str().to_string(),
        role: CardRole::Worker,
        wave_id: Some(boot.wave_id.as_str().to_string()),
        thread_id: "worker-thread".to_string(),
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
    let wave_id = boot.wave_id.clone();
    // PR2 of #247 — every persist_report call now emits TWO envelopes:
    //   1. Event::CardUpdated (generic "row changed" signal — existing PR1 behavior)
    //   2. Event::WaveReportEdited (structured edit-log entry — new in PR2)
    // Subscribe early and collect both so the test can assert order + payload.
    let sub = tokio::spawn(async move { collect_n(&events, 2).await });
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

    // Bus saw exactly two envelopes: CardUpdated first (preserves
    // pre-PR2 broadcast order so the generic "re-fetch" signal lands
    // before the structured edit-log entry), then WaveReportEdited.
    let envs = sub.await.expect("collector ok");
    assert_eq!(
        envs.len(),
        2,
        "expected exactly two envelopes; got {envs:?}"
    );

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
        other => panic!("expected CardUpdated first, got {other:?}"),
    }
    assert!(matches!(envs[0].scope, EventScope::Card { .. }));

    // Second envelope: structured WaveReportEdited.
    match &envs[1].event {
        Event::WaveReportEdited {
            wave_id: w,
            card_id: c,
            author,
            edit_id,
            summary_before,
            summary_after,
            body_before,
            body_after,
        } => {
            assert_eq!(w, &wave_id, "wave_id matches the report card's wave");
            assert_eq!(c, &report_id, "card_id matches the report card");
            // Issue #247 PR3 — the MCP `report.write` / `report.edit`
            // wrapper now passes `EditAuthor::Spec` explicitly (was
            // hard-coded in PR2). REST callers go through the same
            // shared `wave_report::persist_report` but pass
            // `EditAuthor::User` — see `tests/rest_wave_report.rs` for
            // the User-author regression. Spec attribution stays the
            // contract for every spec-MCP write.
            assert_eq!(*author, EditAuthor::Spec, "MCP path tags Spec");
            // edit_id must be a non-empty UUID-shaped string. Don't pin
            // the exact value — it's a fresh UUID per call.
            assert!(!edit_id.is_empty(), "edit_id must be a non-empty UUID");
            // UUID v4 string is 36 chars (8-4-4-4-12 with hyphens).
            assert_eq!(
                edit_id.len(),
                36,
                "edit_id should be a UUID v4 string; got {edit_id:?}",
            );
            // Pre-write state: the seed body + empty summary that
            // `boot()` minted via `WaveReportPayload::initial()`.
            assert_eq!(
                summary_before, "",
                "pre-write summary is the empty initial value",
            );
            assert_eq!(
                body_before, "# Goal\n\n_The spec agent will fill this in._\n",
                "pre-write body is the initial seed body",
            );
            // Post-write state: matches what was passed to report.write.
            assert_eq!(summary_after, "done refactoring");
            assert_eq!(body_after, "# Goal\n\nrefactored everything\n");
        }
        other => panic!("expected WaveReportEdited second, got {other:?}"),
    }
    // Same card scope as the CardUpdated envelope, and the scope row
    // must also populate `scope_wave` + `scope_card` so the dispatcher's
    // push filter can subscribe to the wave's edit log without scanning
    // the firehose.
    match &envs[1].scope {
        EventScope::Card { card, wave, .. } => {
            assert_eq!(card, &report_id, "scope_card persisted on the events row");
            assert_eq!(wave, &wave_id, "scope_wave persisted on the events row");
        }
        other => panic!("expected Card-scoped envelope, got {other:?}"),
    }

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

// ---------------------------------------------------------------------------
// PR2 of #247 — Event::WaveReportEdited coverage.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn edit_emits_wave_report_edited_alongside_card_updated() {
    let boot = boot().await;
    // Seed a known body so the before/after diff is predictable.
    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({ "body": "before XYZ after\n", "summary": "before-summary" }),
    )
    .await
    .expect("seed write");

    // Now subscribe before issuing the edit — we expect TWO envelopes
    // (CardUpdated + WaveReportEdited) from a single `report.edit`
    // call, identical to the `report.write` path.
    let events = boot.ctx.events.clone();
    let report_id = boot.report_card_id.clone();
    let wave_id = boot.wave_id.clone();
    let sub = tokio::spawn(async move { collect_n(&events, 2).await });
    tokio::time::sleep(Duration::from_millis(20)).await;

    call_tool(
        &boot,
        TOOL_REPORT_EDIT,
        spec_identity(&boot),
        json!({ "old_string": "XYZ", "new_string": "ABC" }),
    )
    .await
    .expect("edit succeeds");

    let envs = sub.await.expect("collector ok");
    assert_eq!(
        envs.len(),
        2,
        "expected CardUpdated + WaveReportEdited; got {envs:?}",
    );
    assert!(
        matches!(envs[0].event, Event::CardUpdated(_)),
        "CardUpdated first",
    );
    match &envs[1].event {
        Event::WaveReportEdited {
            wave_id: w,
            card_id: c,
            author,
            edit_id,
            summary_before,
            summary_after,
            body_before,
            body_after,
        } => {
            assert_eq!(w, &wave_id);
            assert_eq!(c, &report_id);
            assert_eq!(*author, EditAuthor::Spec);
            assert_eq!(edit_id.len(), 36, "edit_id is a UUID v4 string");
            // Summary unchanged by report.edit — both before and after
            // are the seeded summary.
            assert_eq!(summary_before, "before-summary");
            assert_eq!(summary_after, "before-summary");
            assert_eq!(body_before, "before XYZ after\n");
            assert_eq!(body_after, "before ABC after\n");
        }
        other => panic!("expected WaveReportEdited, got {other:?}"),
    }
}

#[tokio::test]
async fn write_with_unchanged_content_still_emits_wave_report_edited() {
    // Invariant: every persist_report call → one CardUpdated + one
    // WaveReportEdited. Re-asserting the same body twice produces a
    // second WaveReportEdited with `body_before == body_after`. PR4's
    // UI can filter no-op entries from the timeline if it wants.
    let boot = boot().await;
    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({ "body": "stable body\n", "summary": "stable summary" }),
    )
    .await
    .expect("first write");

    let events = boot.ctx.events.clone();
    let sub = tokio::spawn(async move { collect_n(&events, 2).await });
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Second write with identical body + summary.
    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({ "body": "stable body\n", "summary": "stable summary" }),
    )
    .await
    .expect("second write (content-equal)");

    let envs = sub.await.expect("collector ok");
    assert_eq!(
        envs.len(),
        2,
        "content-equal write still produces both events; got {envs:?}",
    );
    assert!(matches!(envs[0].event, Event::CardUpdated(_)));
    match &envs[1].event {
        Event::WaveReportEdited {
            summary_before,
            summary_after,
            body_before,
            body_after,
            ..
        } => {
            assert_eq!(
                summary_before, summary_after,
                "content-equal write: before == after on summary",
            );
            assert_eq!(
                body_before, body_after,
                "content-equal write: before == after on body",
            );
            assert_eq!(body_before, "stable body\n");
            assert_eq!(summary_before, "stable summary");
        }
        other => panic!("expected WaveReportEdited, got {other:?}"),
    }
}

#[tokio::test]
async fn wave_report_edited_persisted_with_wave_and_card_scope_columns() {
    // The `WaveReportEdited` row must land in the `events` table with
    // `scope_wave = wave_id` and `scope_card = card_id` so the
    // dispatcher's push filter can subscribe to a single wave's edit log
    // without scanning the firehose. Query the table directly through
    // the replay path so
    // we're testing what's persisted, not just what's broadcast.
    let boot = boot().await;
    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({ "body": "scoped body\n", "summary": "scoped summary" }),
    )
    .await
    .expect("write succeeds");

    // Replay every event through the same path the WS handler uses
    // (`events_since`). The tuple shape `(id, version, scope, event)`
    // is reconstructed from the `events.scope_*` columns — so a
    // round-trip back through this path is the strongest assertion
    // available that the row was persisted with the correct scope
    // columns. Filter to the WaveReportEdited rows for the report
    // card and assert the reconstructed scope matches.
    let cursor_rows = boot
        .repo
        .events_since(0, Some(1000))
        .await
        .expect("events_since");
    let edited_rows: Vec<_> = cursor_rows
        .iter()
        .filter(|(_id, _ver, _scope, ev)| matches!(ev, Event::WaveReportEdited { .. }))
        .collect();
    assert_eq!(
        edited_rows.len(),
        1,
        "exactly one WaveReportEdited row persisted; got {edited_rows:?}",
    );
    let (_id, _ver, scope, ev) = edited_rows[0];
    match scope {
        EventScope::Card { card, wave, cove } => {
            assert_eq!(card, &boot.report_card_id, "scope_card");
            assert_eq!(wave, &boot.wave_id, "scope_wave");
            assert!(!cove.as_str().is_empty(), "scope_cove populated");
        }
        other => panic!("expected Card-scoped row, got {other:?}"),
    }
    // Payload round-trips with the spec author + the seed body before /
    // new body after.
    match ev {
        Event::WaveReportEdited {
            author,
            body_before,
            body_after,
            summary_after,
            ..
        } => {
            assert_eq!(*author, EditAuthor::Spec);
            assert_eq!(
                body_before,
                "# Goal\n\n_The spec agent will fill this in._\n"
            );
            assert_eq!(body_after, "scoped body\n");
            assert_eq!(summary_after, "scoped summary");
        }
        other => panic!("expected WaveReportEdited payload, got {other:?}"),
    }
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
async fn edit_with_identical_old_and_new_still_emits_both_events() {
    // Issue #247 PR2 review fix: `report.edit` used to short-circuit
    // when `old_string == new_string` (return early, no write, no
    // event). That broke symmetry with `report.write` — a
    // content-equal `report.write` still emitted both `CardUpdated`
    // and `WaveReportEdited` (see
    // `write_with_unchanged_content_still_emits_wave_report_edited`),
    // while a `report.edit` with equal strings emitted nothing.
    // After the fix every persist path emits exactly the same
    // two-event pair, with `body_before == body_after` and
    // `summary_before == summary_after` for the equal-strings case.
    let boot = boot().await;
    // Seed a known body. The substring "stable" must exist for the
    // post-fix flow to find it (the old `old == new` short-circuit
    // ran *before* the not-found check; now both checks run).
    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({ "body": "stable\n", "summary": "stable-summary" }),
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
    let report_id = boot.report_card_id.clone();
    let wave_id = boot.wave_id.clone();

    // Subscribe — we now expect TWO envelopes from the equal-strings
    // edit, identical to the `report.write` path.
    let events = boot.ctx.events.clone();
    let sub = tokio::spawn(async move { collect_n(&events, 2).await });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let out = call_tool(
        &boot,
        TOOL_REPORT_EDIT,
        spec_identity(&boot),
        json!({ "old_string": "stable", "new_string": "stable" }),
    )
    .await
    .expect("equal-strings edit succeeds (content-equal write)");
    let new_ts = out
        .get("updated_at")
        .and_then(Value::as_i64)
        .expect("updated_at i64");
    assert!(
        new_ts >= before_ts,
        "content-equal edit bumps (or keeps) updated_at; before={before_ts} after={new_ts}",
    );

    // Bus must see exactly two envelopes: CardUpdated then
    // WaveReportEdited, same invariant as `report.write`.
    let envs = sub.await.expect("collector ok");
    assert_eq!(
        envs.len(),
        2,
        "equal-strings edit emits both events (symmetry with report.write); got {envs:?}",
    );
    assert!(
        matches!(envs[0].event, Event::CardUpdated(_)),
        "CardUpdated first (preserves pre-PR2 broadcast order)",
    );
    match &envs[1].event {
        Event::WaveReportEdited {
            wave_id: w,
            card_id: c,
            author,
            edit_id,
            summary_before,
            summary_after,
            body_before,
            body_after,
        } => {
            assert_eq!(w, &wave_id, "wave_id matches");
            assert_eq!(c, &report_id, "card_id matches");
            assert_eq!(*author, EditAuthor::Spec);
            assert_eq!(edit_id.len(), 36, "edit_id is a UUID v4 string");
            // The defining assertion: equal-strings replacement is
            // the identity map, so before == after on both fields.
            assert_eq!(
                body_before, body_after,
                "equal-strings edit: body_before == body_after",
            );
            assert_eq!(
                summary_before, summary_after,
                "equal-strings edit: summary_before == summary_after",
            );
            assert_eq!(body_before, "stable\n");
            assert_eq!(summary_before, "stable-summary");
        }
        other => panic!("expected WaveReportEdited, got {other:?}"),
    }

    // Row's payload is unchanged byte-for-byte (it's the same body).
    let after = boot
        .repo
        .card_get(boot.report_card_id.as_str())
        .await
        .unwrap()
        .unwrap();
    let payload: WaveReportPayload = serde_json::from_value(after.payload).unwrap();
    assert_eq!(payload.body, "stable\n");
    assert_eq!(payload.summary, "stable-summary");
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
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
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
    let spec2_identity = ToolCallIdentity {
        card_id: spec2.id.as_str().to_string(),
        role: CardRole::Spec,
        wave_id: Some(wave2.id.as_str().to_string()),
        thread_id: "spec2-thread".to_string(),
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
