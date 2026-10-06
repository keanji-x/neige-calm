//! #2087 B1c: Calendar's tools are `add`/`ls`/`set`/`rm`. Removal is its own verb, not a
//! `cancelled` flag on an edit; it stops the entry's wakes and has no undo through the tools. The
//! list window is half-open `[from, to)`.
use super::wake::{at, create, cursor, delivered_wakes, planner, rm, set, start_ms, wake_events};
use super::*;
use crate::builtin_plugins::calendar::wake::scan;

async fn call(
    fx: &Fixture,
    who: &ToolCallIdentity,
    tool: &str,
    args: serde_json::Value,
) -> Result<serde_json::Value, crate::mcp_server::framing::RpcError> {
    let registry = crate::mcp_server::build_default_registry();
    let handler = registry.lookup(tool).unwrap();
    handler(fx.ctx.clone(), who.clone(), args)
        .await
        .map(|result| serde_json::to_value(result).unwrap()["structuredContent"].clone())
}

#[tokio::test]
async fn calendar_rm_stops_future_wakes_and_has_no_undo() {
    let fx = Fixture::new().await;
    let planner = planner(&fx).await;
    // Friday 2 and Saturday 3 October, 09:00-10:00 in Shanghai.
    let task = json!({"title":"Open review","description":"","schedule":{
        "kind":"weekly","weekdays":["fri","sat"],"start":"09:00","end":"10:00",
        "timezone":"Asia/Shanghai","from":"2026-10-02"
    }});
    let entry = create(&fx, &planner.identity, "weekly-review", task.clone()).await;
    assert_eq!(
        scan(&fx.ctx, at("2026-10-02T09:00:10+08:00"))
            .await
            .unwrap(),
        1
    );
    let removed = rm(&fx, &planner.identity, &entry).await;
    assert_eq!(removed.version, entry.version + 1);
    assert!(removed.cancelled, "rm sets the stored `cancelled`");

    assert_eq!(
        scan(&fx.ctx, at("2026-10-03T09:00:10+08:00"))
            .await
            .unwrap(),
        0,
        "the next occurrence does not wake"
    );
    assert_eq!(wake_events(&fx).await.len(), 1);
    assert_eq!(
        cursor(&fx, &entry).await,
        Some(start_ms("2026-10-02T09:00:00+08:00")),
        "only the occurrence before the removal was handled"
    );
    assert_eq!(delivered_wakes(&planner.harness, 1).await.len(), 1);
    let listed = call(
        &fx,
        &planner.identity,
        "plugin_calendar_ls",
        json!({"from":"2026-10-01","to":"2026-10-10","timezone":"Asia/Shanghai"}),
    )
    .await
    .unwrap();
    assert_eq!(listed, json!({"entries": []}));

    // Neither a set nor a second rm brings the entry back.
    for (tool, args) in [
        (
            "plugin_calendar_set",
            json!({"entry_id": entry.id, "expected_version": removed.version, "task": task}),
        ),
        (
            "plugin_calendar_rm",
            json!({"entry_id": entry.id, "expected_version": removed.version}),
        ),
    ] {
        let error = call(&fx, &planner.identity, tool, args).await.unwrap_err();
        assert!(error.message.contains("was removed"), "{tool}: {error:?}");
    }
    let stored: Entry = serde_json::from_value(
        fx.repo
            .plugin_kv_get(PLUGIN_ID, &format!("entry:{}", entry.id))
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!((stored.version, stored.cancelled), (removed.version, true));
    assert_eq!(
        scan(&fx.ctx, at("2026-10-09T09:00:10+08:00"))
            .await
            .unwrap(),
        0
    );
    planner.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn calendar_set_edits_without_removing() {
    let fx = Fixture::new().await;
    let who = fx.identity(CardRole::Planner).await;
    let entry = create(&fx, &who, "edit", json!(draft())).await;
    let mut moved = draft();
    moved.title = "Moved".into();
    let edited = set(&fx, &who, &entry, json!(moved)).await;
    assert_eq!(
        (edited.version, edited.cancelled, edited.task.title.as_str()),
        (2, false, "Moved")
    );
}

/// `to` is exclusive: an all-day entry on `to` is outside, on the day before it is inside.
#[tokio::test]
async fn calendar_ls_window_is_half_open() {
    let fx = Fixture::new().await;
    let who = fx.identity(CardRole::Planner).await;
    let entry = create(&fx, &who, "boundary", json!(draft())).await;
    for (from, to, inside) in [
        ("2026-10-01", "2026-10-02", false),
        ("2026-10-02", "2026-10-03", true),
        ("2026-10-01", "2026-10-03", true),
        ("2026-10-03", "2026-10-04", false),
    ] {
        let listed = call(
            &fx,
            &who,
            "plugin_calendar_ls",
            json!({"from": from, "to": to, "timezone": "Asia/Shanghai"}),
        )
        .await
        .unwrap();
        let ids: Vec<_> = listed["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|listed| listed["entry_id"].as_str().unwrap().to_owned())
            .collect();
        let expected = if inside {
            vec![entry.id.clone()]
        } else {
            vec![]
        };
        assert_eq!(ids, expected, "[{from}, {to})");
    }
}

/// The retired `until`, `id` and `cancelled` keys are refused with the valid keys, and nothing is
/// written.
#[tokio::test]
async fn calendar_tools_refuse_retired_keys_with_the_valid_keys() {
    let fx = Fixture::new().await;
    let who = fx.identity(CardRole::Planner).await;
    let entry = create(&fx, &who, "closed", json!(draft())).await;
    let task = json!(draft());
    for (tool, args, refused) in [
        (
            "plugin_calendar_ls",
            json!({"from":"2026-10-02","until":"2026-10-03","timezone":"Asia/Shanghai"}),
            "plugin_calendar_ls: unknown argument `until`; valid: from, timezone, to",
        ),
        (
            "plugin_calendar_set",
            json!({"id": entry.id, "expected_version": 1, "task": task}),
            "plugin_calendar_set: unknown argument `id`; valid: entry_id, expected_version, task",
        ),
        (
            "plugin_calendar_set",
            json!({"entry_id": entry.id, "expected_version": 1, "task": task,
                "cancelled": false}),
            "plugin_calendar_set: unknown argument `cancelled`; valid: entry_id, expected_version, \
             task",
        ),
        (
            "plugin_calendar_rm",
            json!({"entry_id": entry.id, "expected_version": 1, "cancelled": true}),
            "plugin_calendar_rm: unknown argument `cancelled`; valid: entry_id, expected_version",
        ),
        (
            "plugin_calendar_rm",
            json!({"id": entry.id, "expected_version": 1}),
            "plugin_calendar_rm: unknown argument `id`; valid: entry_id, expected_version",
        ),
    ] {
        let error = call(&fx, &who, tool, args).await.unwrap_err();
        assert_eq!(
            (error.code, error.message.as_str()),
            (-32602, refused),
            "{tool}"
        );
    }
    let stored = store::list(&fx.ctx, &human(), window()).await.unwrap();
    assert_eq!(
        stored
            .iter()
            .map(|listed| (listed.entry.version, listed.entry.cancelled))
            .collect::<Vec<_>>(),
        vec![(1, false)]
    );
}
