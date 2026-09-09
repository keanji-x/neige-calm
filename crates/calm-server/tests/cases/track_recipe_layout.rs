//! #1595: exercise Recipe storage and Track creation, not a copied parser.
use super::*;

fn layout() -> Value {
    json!({"version":1,"columns":1,"gap":"normal","surface":"plain","items":[{
        "kind":"table","title":"Holdings","span":1,
        "data":{"source":"neige://plugin/market/holdings","annotations":{"keys":["asset","venue"],"rows":[]}},
        "columns":[{"key":"asset","label":"Asset","format":"text","digits":0}]
    }]})
}

#[tokio::test]
async fn layout_recipe_normalizes_instantiates_and_copies_configuration() {
    let boot = boot().await;
    let layout = layout();
    let body = format!("# Dashboard\n\n```neige-block layout\n{}\n```\n", layout);
    let recipe = create_recipe(boot.app.clone(), "Reusable dashboard", &body).await;
    assert!(
        recipe["body"]
            .as_str()
            .unwrap()
            .contains(&calm_types::report_blocks::render_fence("layout", &layout))
    );
    let mut tracks = Vec::new();
    for title in ["layout-first", "layout-second"] {
        let (status, created) = send(
            boot.app.clone(),
            "POST",
            "/api/tracks",
            Some(create_track_body(
                &boot.area_id,
                title,
                json!({"recipe_id":recipe["id"]}),
            )),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let id = created["id"].as_str().unwrap().to_string();
        let payload = report_payload(&track_detail(boot.app.clone(), &id).await);
        let block = payload
            .blocks
            .as_ref()
            .unwrap()
            .iter()
            .find(|b| b.kind == "layout")
            .expect("layout block persisted");
        assert_eq!(block.payload, layout);
        assert_eq!(
            block.payload["items"][0]["data"]["annotations"]["rows"],
            json!([])
        );
        tracks.push(id);
    }
    let mut edited = layout.clone();
    edited["gap"] = json!("wide");
    let(status,result)=send(boot.app.clone(),"PUT",&format!("/api/track-recipes/{}",recipe["id"].as_str().unwrap()),Some(json!({"title":"Edited template","body":calm_types::report_blocks::render_fence("layout",&edited),"if_revision":1}))).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    for id in tracks {
        let payload = report_payload(&track_detail(boot.app.clone(), &id).await);
        assert_eq!(
            payload
                .blocks
                .unwrap()
                .into_iter()
                .find(|b| b.kind == "layout")
                .unwrap()
                .payload,
            layout,
            "existing reports are value copies"
        );
    }
}

#[tokio::test]
async fn layout_recipe_create_and_update_reject_invalid_configuration() {
    let boot = boot().await;
    let recipe = create_recipe(
        boot.app.clone(),
        "valid",
        &calm_types::report_blocks::render_fence("layout", &layout()),
    )
    .await;
    let mut invalid = layout();
    invalid["items"][0]["span"] = json!(2);
    for (method, url, revision) in [
        ("POST", "/api/track-recipes".to_string(), None),
        (
            "PUT",
            format!("/api/track-recipes/{}", recipe["id"].as_str().unwrap()),
            Some(1),
        ),
    ] {
        let mut request = json!({"title":"invalid","body":calm_types::report_blocks::render_fence("layout",&invalid)});
        if let Some(revision) = revision {
            request["if_revision"] = json!(revision);
        }
        let (status, response) = send(boot.app.clone(), method, &url, Some(request)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
        assert!(response.to_string().contains("span"), "{response}");
    }
}

#[tokio::test]
async fn layout_shipped_portfolio_recipe_instantiates_every_saved_component() {
    // Read the exact file offered by the Recipes UI; no second example copy.
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fe/web/src/features/report/recipe/examples/portfolio.md");
    let body = std::fs::read_to_string(path).expect("shipped portfolio Recipe");
    let boot = boot().await;
    let recipe = create_recipe(boot.app.clone(), "投资组合", &body).await;
    let stored = recipe["body"].as_str().unwrap();
    let expected: Vec<_> = calm_types::report_blocks::split_body(&body)
        .iter()
        .filter_map(|slice| calm_types::report_blocks::parse_fence(&slice.raw))
        .collect();
    assert_eq!(
        expected.len(),
        3,
        "three saved sections in the offered template"
    );
    for fence in &expected {
        assert_eq!(fence.kind, "layout");
        assert!(stored.contains(&calm_types::report_blocks::render_fence(
            &fence.kind,
            &fence.payload
        )));
        for item in fence.payload["items"].as_array().unwrap() {
            for rows in [
                item.pointer("/data/rows"),
                item.pointer("/data/annotations/rows"),
            ]
            .into_iter()
            .flatten()
            {
                assert_eq!(
                    rows,
                    &json!([]),
                    "reusable templates contain no investor rows"
                );
            }
        }
    }
    let (status, created) = send(
        boot.app.clone(),
        "POST",
        "/api/tracks",
        Some(create_track_body(
            &boot.area_id,
            "shipped-portfolio-layout",
            json!({"recipe_id":recipe["id"]}),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let report =
        report_payload(&track_detail(boot.app.clone(), created["id"].as_str().unwrap()).await);
    assert_eq!(
        report.body, stored,
        "normalization and instantiation preserve the entire canonical body"
    );
    let actual: Vec<_> = report
        .blocks
        .as_ref()
        .unwrap()
        .iter()
        .filter(|block| block.kind == "layout")
        .map(|block| &block.payload)
        .collect();
    assert_eq!(
        actual,
        expected
            .iter()
            .map(|fence| &fence.payload)
            .collect::<Vec<_>>()
    );
}
