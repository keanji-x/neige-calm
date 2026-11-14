use super::*;

#[test]
fn canonical_segments_preserve_names_and_reject_noncanonical_aliases() {
    let siblings = [
        ("", "abcdefgh-one"),
        ("", "abcdefgh-two"),
        ("重名", "12345678a"),
        ("重名", "12345678b"),
        (".neige", "system-name"),
        (" a/%~\\\n ", "special"),
    ];
    let names = name::segments(&siblings);
    assert_eq!(
        names,
        [
            "~abcdefgh-o",
            "~abcdefgh-t",
            "重名~12345678a",
            "重名~12345678b",
            "%2Eneige",
            " a%2F%25%7E%5C%0A "
        ]
    );
    for (index, segment) in names.iter().enumerate() {
        assert_eq!(lookup(segment, &siblings).unwrap(), siblings[index].1);
    }
    for bad in [
        "",
        "重名",
        "重名~12345678",
        "重名~12345678aaaa",
        "%2eneige",
        ".neige",
        "%41",
        " a%2f%25%7E%5C%0A ",
    ] {
        assert_eq!(lookup(bad, &siblings).unwrap_err().code, -32602, "{bad}");
    }
    assert_eq!(name::unescape("%252F").unwrap(), "%2F", "single decode");
    let renamed = [("new", "12345678a")];
    assert!(lookup(&names[2], &renamed).is_err());
    assert!(lookup("new~12345678", &renamed).is_err());
    assert_eq!(lookup("new", &renamed).unwrap(), "12345678a");
}

#[tokio::test]
async fn navigation_keeps_bound_system_identity_and_checks_each_intermediate() {
    use axum::extract::FromRef;
    let (_tmp, _repo, state) = crate::daily_planner::tests::fixture().await;
    let route = crate::state::RouteState::from_ref(&state);
    let own = crate::daily_planner::reconcile(
        &route,
        crate::daily_planner::tests::at("2026-10-04T01:00:00Z"),
    )
    .await
    .unwrap();
    let card = route
        .repo
        .cards_by_track(own.id.as_str())
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.kind == "codex")
        .unwrap();
    let identity = ToolCallIdentity {
        card_id: card.id.to_string(),
        role: crate::model::CardRole::Planner,
        provider: calm_types::runtime::AgentProvider::Codex,
        session_id: "paths-test".into(),
        track_id: Some(own.id.to_string()),
        area_id: own.area_id.to_string(),
        thread_id: "bound".into(),
    };
    let ctx = &route.mcp_context;
    for path in [".", "./", "guide/..", "cards/.."] {
        assert_eq!(
            resolve(ctx, &identity, &own, path).await.unwrap(),
            Scope::Bound(String::new()),
            "{path}"
        );
    }
    assert_eq!(
        resolve(ctx, &identity, &own, "../..").await.unwrap(),
        Scope::Root
    );
    assert_eq!(
        resolve(ctx, &identity, &own, "report.md").await.unwrap(),
        Scope::Report(own.area_id.to_string(), own.id.to_string())
    );
    assert_eq!(
        resolve(ctx, &identity, &own, "..").await.unwrap_err().code,
        -32403
    );
    assert_eq!(
        resolve(ctx, &identity, &own, "../other")
            .await
            .unwrap_err()
            .code,
        -32403
    );
    for path in [
        "report.md/..",
        "guide/report.md/..",
        "unknown/..",
        "guide/unknown/..",
        "../../../",
        "//",
        "cards//x",
        "report.md/",
    ] {
        assert_eq!(
            resolve(ctx, &identity, &own, path).await.unwrap_err().code,
            -32602,
            "{path}"
        );
    }
    assert_eq!(
        resolve(
            ctx,
            &identity,
            &own,
            "/.neige/report-changes/2026-10-03.json"
        )
        .await
        .unwrap(),
        Scope::Changes("2026-10-03.json".into())
    );
    for path in [
        "/report-changes/2026-10-03.json",
        "/areas/no/tracks/no/report.md",
        "area/reports/x.md",
        "area/reports/../x",
        "area/reports/a/b.md",
        "area/reports//x.md",
        "area/../report.md",
        "area//reports",
        "area/other",
        "area/reports/",
    ] {
        assert_eq!(
            resolve(ctx, &identity, &own, path).await.unwrap_err().code,
            -32602,
            "{path}"
        );
    }
}
