use super::*;
use calm_server::model::AreaPatch;

#[tokio::test]
async fn encoded_area_and_track_names_rename_without_aliases() {
    let boot = boot(&[(0, " a/%~\\\n "), (1, "plain")]).await;
    let own = &boot.sides[0];
    boot.sqlx
        .area_update(
            own.area_id.as_str(),
            AreaPatch {
                name: Some(".neige".into()),
                ..AreaPatch::default()
            },
        )
        .await
        .unwrap();
    let path = "/%2Eneige/ a%2F%25%7E%5C%0A /report.md";
    assert_eq!(
        cat(&boot, planner(own), path).await.unwrap(),
        cat(&boot, planner(own), "report.md").await.unwrap()
    );
    let abs = "/%2Eneige/ a%2F%25%7E%5C%0A /cards/index.json";
    assert_eq!(
        cat(&boot, planner(own), abs).await.unwrap(),
        cat(&boot, planner(own), "cards/index.json").await.unwrap()
    );
    for bad in [
        "/%2eneige/ a%2F%25%7E%5C%0A /report.md",
        "/area-a/ a%2F%25%7E%5C%0A /report.md",
        "/.neige/ a%2F%25%7E%5C%0A /report.md",
        "/%252Eneige/ a%2F%25%7E%5C%0A /report.md",
    ] {
        assert_eq!(
            cat(&boot, planner(own), bad).await.unwrap_err().code,
            INVALID_PARAMS,
            "{bad}"
        );
    }
    boot.sqlx
        .area_update(
            boot.areas[1].as_str(),
            AreaPatch {
                name: Some(".neige".into()),
                ..AreaPatch::default()
            },
        )
        .await
        .unwrap();
    let root = ls(&boot, planner(own), "/").await.unwrap();
    let area = root["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["area_id"] == own.area_id.as_str())
        .unwrap();
    let renamed = format!(
        "{} a%2F%25%7E%5C%0A /report.md",
        area["path"].as_str().unwrap()
    );
    assert_eq!(
        cat(&boot, planner(own), path).await.unwrap_err().code,
        INVALID_PARAMS
    );
    assert!(cat(&boot, planner(own), &renamed).await.is_ok());
    // "areas" is an ordinary user name, not an old protocol alias.
    boot.sqlx
        .area_update(
            boot.areas[1].as_str(),
            AreaPatch {
                name: Some("areas".into()),
                ..AreaPatch::default()
            },
        )
        .await
        .unwrap();
    assert!(
        cat(&boot, planner(own), "/areas/plain/report.md")
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn directory_cursors_follow_identity_across_spelling_and_rename() {
    let titles: Vec<_> = (0..55).map(|i| format!("track {i:02}")).collect();
    let specs: Vec<_> = titles.iter().map(|t| (0, t.as_str())).collect();
    let boot = boot(&specs).await;
    let reader = boot
        .sides
        .iter()
        .min_by(|a, b| a.track_id.as_str().cmp(b.track_id.as_str()))
        .unwrap();
    let first = ls(&boot, planner(reader), "..").await.unwrap();
    let cursor = first["next_cursor"].as_str().expect("more than 50 rows");
    let args = json!({"path":"/area-a/","cursor":cursor});
    let continued = call(&boot, TOOL_LS, planner(reader), args.clone())
        .await
        .unwrap();
    let relative = call(
        &boot,
        TOOL_LS,
        planner(reader),
        json!({"path":"..","cursor":cursor}),
    )
    .await
    .unwrap();
    assert_eq!(continued, relative);
    assert_eq!(
        first["entries"].as_array().unwrap().len() + continued["entries"].as_array().unwrap().len(),
        55
    );
    for path in ["/", "/area-b/", "/.neige/"] {
        assert_eq!(
            call(
                &boot,
                TOOL_LS,
                planner(reader),
                json!({"path":path,"cursor":cursor})
            )
            .await
            .unwrap_err()
            .code,
            INVALID_PARAMS,
            "{path}"
        );
    }
    boot.sqlx
        .area_update(
            reader.area_id.as_str(),
            AreaPatch {
                name: Some("renamed".into()),
                ..AreaPatch::default()
            },
        )
        .await
        .unwrap();
    let renamed = call(
        &boot,
        TOOL_LS,
        planner(reader),
        json!({"path":"/renamed/","cursor":cursor}),
    )
    .await
    .unwrap();
    assert_eq!(
        renamed["entries"].as_array().unwrap().len(),
        continued["entries"].as_array().unwrap().len()
    );
    assert!(
        renamed["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["path"].as_str().unwrap().starts_with("/renamed/"))
    );
    let last = first["entries"].as_array().unwrap().last().unwrap()["track_id"]
        .as_str()
        .unwrap();
    // Keep the bound identity alive while deleting the cursor row.
    assert_ne!(last, reader.track_id.as_str());
    {
        boot.sqlx.track_delete(last).await.unwrap();
        assert_eq!(
            call(
                &boot,
                TOOL_LS,
                planner(reader),
                json!({"path":"..","cursor":cursor})
            )
            .await
            .unwrap_err()
            .code,
            INVALID_PARAMS
        );
    }
}
