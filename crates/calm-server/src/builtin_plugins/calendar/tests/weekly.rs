//! Weekly entries: created and listed through the production tools, woken by the scan at fixed
//! instants, delivered by a real Dispatcher.
use super::wake::{at, create, cursor, delivered_wakes, planner, start_ms, wake_events};
use super::*;
use crate::builtin_plugins::calendar::wake::scan;
use crate::event::Event;
use serde_json::Value;

fn weekly(weekdays: &[&str], start: &str, end: &str, zone: &str, from: &str) -> Value {
    json!({"title":"Open review","description":"","schedule":{
        "kind":"weekly","weekdays":weekdays,"start":start,"end":end,"timezone":zone,"from":from
    }})
}

fn until(mut task: Value, last: &str) -> Value {
    task["schedule"]["until"] = json!(last);
    task
}

async fn try_create(fx: &Fixture, who: &ToolCallIdentity, task: Value) -> Result<Value, String> {
    let registry = crate::mcp_server::build_default_registry();
    let create = registry.lookup("neige_calendar_create").unwrap();
    create(
        fx.ctx.clone(),
        who.clone(),
        json!({"idempotency_key": task.to_string(), "task": task}),
    )
    .await
    .map(|result| serde_json::to_value(result).unwrap()["structuredContent"].clone())
    .map_err(|error| format!("{error:?}"))
}

/// The list tool's projection: each listed entry id with its occurrence starts.
async fn listed(
    fx: &Fixture,
    who: &ToolCallIdentity,
    from: &str,
    until: &str,
    zone: &str,
) -> Vec<(String, Vec<String>)> {
    let registry = crate::mcp_server::build_default_registry();
    let list = registry.lookup("neige_calendar_list").unwrap();
    let result = list(
        fx.ctx.clone(),
        who.clone(),
        json!({"from": from, "until": until, "timezone": zone}),
    )
    .await
    .unwrap();
    let entries = serde_json::to_value(result).unwrap()["structuredContent"].clone();
    entries
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            let starts = entry["occurrences"].as_array().unwrap().iter();
            (
                entry["id"].as_str().unwrap().to_owned(),
                starts
                    .map(|span| span["start"].as_str().unwrap().to_owned())
                    .collect(),
            )
        })
        .collect()
}

fn texts(events: Vec<Event>) -> Vec<String> {
    events
        .into_iter()
        .map(|event| match event {
            Event::TrackWakeRequested { text, .. } => text,
            other => panic!("unexpected {other:?}"),
        })
        .collect()
}

#[tokio::test]
async fn weekly_entry_lists_each_occurrence_from_its_first_through_its_last_date() {
    let fx = Fixture::new().await;
    let who = fx.identity(CardRole::Planner).await;
    let task = until(
        weekly(
            &["mon", "wed", "fri"],
            "09:30",
            "10:00",
            "Asia/Shanghai",
            "2026-10-05",
        ),
        "2026-10-14",
    );
    let entry = create(&fx, &who, "series", task.clone()).await;
    assert_eq!(serde_json::to_value(&entry.task).unwrap(), task);

    let at_930 = |day: &str| format!("2026-10-{day}T09:30:00+08:00");
    assert_eq!(
        listed(&fx, &who, "2026-10-01", "2026-10-22", "Asia/Shanghai").await,
        vec![(
            entry.id.clone(),
            ["05", "07", "09", "12", "14"].map(at_930).to_vec()
        )],
        "Friday 2 precedes `from`; Friday 16 follows `until`"
    );
    assert_eq!(
        listed(&fx, &who, "2026-10-15", "2026-10-22", "Asia/Shanghai").await,
        vec![],
        "a window without an occurrence omits the entry"
    );
    // Monday 09:30 in Shanghai is Sunday evening in Los Angeles.
    assert_eq!(
        listed(&fx, &who, "2026-10-04", "2026-10-05", "America/Los_Angeles").await,
        vec![(entry.id.clone(), vec![at_930("05")])]
    );
    assert_eq!(
        listed(&fx, &who, "2026-10-05", "2026-10-06", "America/Los_Angeles").await,
        vec![]
    );

    let open = create(
        &fx,
        &who,
        "open-ended",
        weekly(&["sun"], "18:00", "19:00", "Asia/Shanghai", "2026-10-04"),
    )
    .await;
    let stored = fx
        .repo
        .plugin_kv_get(PLUGIN_ID, &format!("entry:{}", open.id))
        .await
        .unwrap()
        .unwrap();
    assert!(stored["task"]["schedule"].get("until").is_none());
    assert_eq!(
        listed(&fx, &who, "2027-10-01", "2027-10-04", "Asia/Shanghai").await,
        vec![(open.id, vec!["2027-10-03T18:00:00+08:00".to_owned()])]
    );
}

#[tokio::test]
async fn weekly_entry_wakes_once_per_occurrence_on_consecutive_days() {
    let fx = Fixture::new().await;
    let planner = planner(&fx).await;
    let entry = create(
        &fx,
        &planner.identity,
        "daily",
        weekly(
            &["mon", "tue"],
            "09:00",
            "10:00",
            "Asia/Shanghai",
            "2026-10-05",
        ),
    )
    .await;

    for (now, woken) in [
        ("2026-10-05T08:59:59+08:00", 0),
        ("2026-10-05T09:00:10+08:00", 1),
        ("2026-10-05T09:01:00+08:00", 0),
        ("2026-10-06T08:59:59+08:00", 0),
        ("2026-10-06T09:00:10+08:00", 1),
        ("2026-10-06T09:30:00+08:00", 0),
        // Wednesday is not a listed weekday.
        ("2026-10-07T09:00:10+08:00", 0),
    ] {
        assert_eq!(scan(&fx.ctx, at(now)).await.unwrap(), woken, "at {now}");
    }
    assert_eq!(
        texts(wake_events(&fx).await),
        ["05", "06"].map(|day| format!(
            "Calendar entry \"Open review\" started at 2026-10-{day} 09:00 Asia/Shanghai and \
             ends at 2026-10-{day} 10:00 (on time). Do what this Track scheduled it for."
        ))
    );
    assert_eq!(
        cursor(&fx, &entry).await,
        Some(start_ms("2026-10-06T09:00:00+08:00"))
    );
    assert_eq!(delivered_wakes(&planner.harness, 2).await.len(), 2);
    planner.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn missed_weekly_occurrence_fires_late_only_before_its_end() {
    let fx = Fixture::new().await;
    let planner = planner(&fx).await;
    let days = ["mon", "wed"];
    let running = create(
        &fx,
        &planner.identity,
        "running",
        weekly(&days, "09:00", "10:00", "Asia/Shanghai", "2026-10-05"),
    )
    .await;
    let ended = create(
        &fx,
        &planner.identity,
        "ended",
        weekly(&days, "08:00", "09:00", "Asia/Shanghai", "2026-10-05"),
    )
    .await;

    // The first scan in days: Monday's occurrences and Wednesday's 08:00 one are over.
    assert_eq!(
        scan(&fx.ctx, at("2026-10-07T09:30:00+08:00"))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        scan(&fx.ctx, at("2026-10-07T09:31:00+08:00"))
            .await
            .unwrap(),
        0
    );
    match wake_events(&fx).await.as_slice() {
        [Event::TrackWakeRequested { key, text, .. }] => {
            assert_eq!(key, &running.id);
            assert!(
                text.contains("started at 2026-10-07 09:00 Asia/Shanghai")
                    && text.contains("(30 min late)"),
                "{text}"
            );
        }
        other => panic!("expected only Wednesday's running occurrence to wake, got {other:?}"),
    }
    assert_eq!(
        cursor(&fx, &ended).await,
        Some(start_ms("2026-10-07T08:00:00+08:00")),
        "the latest ended occurrence is handled without a wake"
    );
    assert_eq!(delivered_wakes(&planner.harness, 1).await.len(), 1);
    planner.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn weekly_occurrences_keep_their_wall_time_across_dst() {
    let fx = Fixture::new().await;
    let planner = planner(&fx).await;
    let zone = "America/New_York";
    // New York falls back on Sunday 2026-11-01: 01:30 happens twice.
    let fall = create(
        &fx,
        &planner.identity,
        "fall",
        until(
            weekly(&["sat", "sun", "mon"], "01:30", "01:45", zone, "2026-10-31"),
            "2026-11-02",
        ),
    )
    .await;
    // It springs forward on Sunday 2026-03-08: 02:30 does not exist.
    let spring = create(
        &fx,
        &planner.identity,
        "spring",
        until(
            weekly(&["sun"], "02:30", "03:15", zone, "2026-03-08"),
            "2026-03-15",
        ),
    )
    .await;
    assert_eq!(
        listed(&fx, &planner.identity, "2026-10-31", "2026-11-03", zone).await,
        vec![(
            fall.id.clone(),
            vec![
                "2026-10-31T01:30:00-04:00".to_owned(),
                "2026-11-01T01:30:00-04:00".to_owned(),
                "2026-11-02T01:30:00-05:00".to_owned(),
            ]
        )]
    );
    assert_eq!(
        listed(&fx, &planner.identity, "2026-03-08", "2026-03-16", zone).await,
        vec![(spring.id, vec!["2026-03-15T02:30:00-04:00".to_owned()])],
        "the occurrence in the spring-forward gap is skipped"
    );

    for (now, woken) in [
        ("2026-11-01T05:30:10Z", 1),
        // 01:30 again, now in EST: the same occurrence does not wake twice.
        ("2026-11-01T06:30:10Z", 0),
        ("2026-11-02T06:29:59Z", 0),
        ("2026-11-02T06:30:10Z", 1),
    ] {
        assert_eq!(scan(&fx.ctx, at(now)).await.unwrap(), woken, "at {now}");
    }
    let woken = texts(wake_events(&fx).await);
    assert_eq!(woken.len(), 2, "{woken:?}");
    assert!(woken[0].contains("started at 2026-11-01 01:30 America/New_York"));
    assert!(woken[1].contains("started at 2026-11-02 01:30 America/New_York"));
    assert_eq!(
        cursor(&fx, &fall).await,
        Some(start_ms("2026-11-02T06:30:00Z"))
    );
    planner.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn invalid_weekly_shapes_are_rejected() {
    let fx = Fixture::new().await;
    let who = fx.identity(CardRole::Planner).await;
    let valid = weekly(&["mon"], "09:00", "10:00", "Asia/Shanghai", "2026-10-05");
    try_create(&fx, &who, valid.clone()).await.unwrap();
    let with = |field: &str, value: Value| {
        let mut task = valid.clone();
        task["schedule"][field] = value;
        task
    };
    for task in [
        with("weekdays", json!([])),
        with("weekdays", json!(["mon", "mon"])),
        with("weekdays", json!(["monday"])),
        with("start", json!("9:00")),
        with("start", json!("10:00")),
        with("end", json!("24:00")),
        with("timezone", json!("Mars/Base")),
        with("from", json!("2026-02-30")),
        with("until", json!("2026-10-04")),
        with("every", json!(2)),
    ] {
        let schedule = task["schedule"].clone();
        assert!(
            try_create(&fx, &who, task).await.is_err(),
            "accepted {schedule}"
        );
    }
}
