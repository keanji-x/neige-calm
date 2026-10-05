//! `neige_track_cat { blocks }` (#1874, `neige track cat <report> --blocks`): chosen blocks of a report,
//! own (`report.md`) or same-area (`area/reports/<name>.md`), exactly as `neige_report_read`'s
//! `select.blocks` text. Block ids come from `neige_area_ls`, the Planner's id source.

use super::*;
use calm_server::mcp_server::tools::report_links::TOOL_AREA_LS;

/// A maintenance contract before the first heading (block 0), then three sections.
const BODY: &str = "<!-- contract: keep the conclusion first -->\nContract intro.\n\n# Goal\n\nalpha\n\n# Findings\n\nbeta\n\n# Next\n\ngamma\n";

async fn cat_blocks(
    boot: &Boot,
    who: ToolCallIdentity,
    path: &str,
    blocks: &[&str],
) -> Result<String, RpcError> {
    let value = call(
        boot,
        TOOL_TRACK_CAT,
        who,
        json!({ "path": path, "blocks": blocks }),
    )
    .await?;
    assert_eq!(value["content_type"], "text/markdown", "{value}");
    Ok(value["content"].as_str().expect("content").to_string())
}

/// `side`'s block ids in document order, as `neige_area_ls` lists them to `reader`.
async fn outline_ids(boot: &Boot, reader: &Side, side: &Side) -> Vec<String> {
    let outline = call(boot, TOOL_AREA_LS, planner(reader), json!({}))
        .await
        .expect("neige_area_ls");
    let track = outline["tracks"]
        .as_array()
        .expect("tracks")
        .iter()
        .find(|track| track["id"] == side.track_id.as_str())
        .expect("the track is outlined");
    track["blocks"]
        .as_array()
        .expect("blocks")
        .iter()
        .map(|block| block["id"].as_str().expect("id").to_string())
        .collect()
}

async fn select_text(boot: &Boot, side: &Side, blocks: &[&str]) -> String {
    let value = call(
        boot,
        TOOL_REPORT_READ,
        planner(side),
        json!({ "select": { "blocks": blocks } }),
    )
    .await
    .expect("neige_report_read");
    value["text"].as_str().expect("text").to_string()
}

#[tokio::test]
async fn blocks_of_another_same_area_report_come_back_in_document_order_with_markers_only() {
    let boot = boot(&[(0, "认证 方案"), (0, "登录 排查")]).await;
    let (own, other) = (&boot.sides[0], &boot.sides[1]);
    write_body(&boot, other, BODY).await;
    let ids = outline_ids(&boot, own, other).await;
    assert_eq!(ids.len(), 4, "contract + three sections: {ids:?}");

    let text = cat_blocks(
        &boot,
        planner(own),
        "area/reports/登录 排查.md",
        &[&ids[3], &ids[1]],
    )
    .await
    .unwrap();
    assert_eq!(
        text,
        format!(
            "<!-- neige:{} -->\n# Goal\n\nalpha\n\n<!-- neige:{} -->\n# Next\n\ngamma\n",
            ids[1], ids[3]
        ),
        "document order, each block after its marker line, nothing else"
    );
    assert!(
        !text.contains("contract"),
        "block 0 is absent unless named: {text}"
    );
}

/// One report, one output: the own `report.md`, the same report through `area/reports/` and the
/// report's own `neige_report_read` select all print the same bytes.
#[tokio::test]
async fn blocks_text_equals_calm_report_read_select_on_every_report_path() {
    let boot = boot(&[(0, "认证 方案"), (0, "登录 排查")]).await;
    let (own, other) = (&boot.sides[0], &boot.sides[1]);
    // A second, id-preserving write: the kept blocks keep ids a fresh split of the body would not mint.
    let edited = BODY.replace("# Goal", "# Extra\n\nx\n\n# Goal");
    for side in [own, other] {
        write_body(&boot, side, BODY).await;
    }
    let first = outline_ids(&boot, own, own).await;
    for side in [own, other] {
        write_body(&boot, side, &edited).await;
    }

    let ids = outline_ids(&boot, own, own).await;
    let kept: Vec<_> = ids
        .iter()
        .filter(|id| id.as_str() != ids[1].as_str())
        .cloned()
        .collect();
    assert_eq!(
        kept, first,
        "the second write keeps every surviving block's id"
    );
    let chosen = [ids[4].as_str(), ids[2].as_str()];
    let want = select_text(&boot, own, &chosen).await;
    assert!(
        want.starts_with(&format!("<!-- neige:{} -->\n# Goal", ids[2])),
        "{want}"
    );
    assert_eq!(
        cat_blocks(&boot, planner(own), "report.md", &chosen)
            .await
            .unwrap(),
        want
    );
    assert_eq!(
        cat_blocks(&boot, planner(own), "/report.md", &chosen)
            .await
            .unwrap(),
        want
    );
    assert_eq!(
        cat_blocks(&boot, planner(own), "area/reports/认证 方案.md", &chosen)
            .await
            .unwrap(),
        want
    );
    // A Worker reads its own report's blocks too; only `area/` is the Planner's.
    assert_eq!(
        cat_blocks(&boot, who(own, CardRole::Worker), "report.md", &chosen)
            .await
            .unwrap(),
        want
    );

    let theirs = outline_ids(&boot, own, other).await;
    let chosen = [theirs[2].as_str(), theirs[4].as_str()];
    assert_eq!(
        cat_blocks(&boot, planner(own), "area/reports/登录 排查.md", &chosen)
            .await
            .unwrap(),
        select_text(&boot, other, &chosen).await,
        "another track's blocks read exactly as that track's own select"
    );
}

#[tokio::test]
async fn an_unknown_block_id_is_refused_with_the_reports_block_list() {
    let boot = boot(&[(0, "认证 方案"), (0, "登录 排查")]).await;
    let (own, other) = (&boot.sides[0], &boot.sides[1]);
    write_body(&boot, own, BODY).await;
    write_body(&boot, other, BODY).await;
    for (reader, path, side) in [
        (own, "report.md", own),
        (own, "area/reports/登录 排查.md", other),
    ] {
        let ids = outline_ids(&boot, reader, side).await;
        let err = cat_blocks(&boot, planner(reader), path, &[&ids[1], "b_nope"])
            .await
            .expect_err("unknown id");
        assert_eq!(err.code, INVALID_PARAMS, "{err:?}");
        assert!(
            err.message.contains("unknown block id `b_nope`"),
            "{path}: {err:?}"
        );
        let listed = format!(
            "\n  {}  Contract intro.\n  {}  Goal\n  {}  Findings\n  {}  Next",
            ids[0], ids[1], ids[2], ids[3]
        );
        assert!(
            err.message.ends_with(&listed),
            "{path}: lists `<id>  <heading>` per block: {err:?}"
        );
    }
}

#[tokio::test]
async fn blocks_on_a_path_that_names_no_report_is_refused() {
    let boot = boot(&[(0, "认证 方案"), (0, "登录 排查")]).await;
    let side = &boot.sides[0];
    for path in [
        "index.md",
        "track.json",
        "runs/index.json",
        "runs/x.md",
        "cards/index.json",
        "/",
        "area",
        "area/reports/",
        "area/reports/a/b.md",
        "area/other",
    ] {
        assert_refused(
            cat_blocks(&boot, planner(side), path, &["b_1"]).await,
            INVALID_PARAMS,
            "is not a report; --blocks reads blocks of `report.md` or `area/reports/<name>.md` only",
        );
    }
    let worker = || who(side, CardRole::Worker);
    assert_refused(
        cat_blocks(&boot, worker(), "runs/index.json", &["b_1"]).await,
        INVALID_PARAMS,
        "is not a report",
    );
    for path in ["area/reports/登录 排查.md", "area/reports/"] {
        assert_refused(
            cat_blocks(&boot, worker(), path, &["b_1"]).await,
            FORBIDDEN,
            "Planner's view",
        );
    }
    for blocks in [json!([]), json!("b_1"), json!([1]), json!({})] {
        assert_refused(
            call(
                &boot,
                TOOL_TRACK_CAT,
                planner(side),
                json!({ "path": "report.md", "blocks": blocks }),
            )
            .await,
            INVALID_PARAMS,
            "`blocks` must be a non-empty array of block ids",
        );
    }
}

#[tokio::test]
async fn ambiguous_and_cross_area_names_are_still_refused_with_blocks() {
    let boot = boot(&[
        (0, "认证 方案"),
        (0, "认证 方案"),
        (0, "登录 排查"),
        (1, "机密"),
    ])
    .await;
    let (first, reader, secret) = (&boot.sides[0], &boot.sides[2], &boot.sides[3]);
    write_body(&boot, first, BODY).await;
    write_body(&boot, secret, BODY).await;
    let ids = outline_ids(&boot, reader, first).await;

    let err = cat_blocks(
        &boot,
        planner(reader),
        "area/reports/认证 方案.md",
        &[&ids[1]],
    )
    .await
    .expect_err("ambiguous");
    assert_eq!(err.code, INVALID_PARAMS, "{err:?}");
    assert!(err.message.contains("names 2 reports"), "{err:?}");

    let secret_ids = outline_ids(&boot, secret, secret).await;
    for path in [
        "area/reports/机密.md".to_string(),
        format!("area/reports/机密~{}.md", &secret.track_id.as_str()[..8]),
        format!("area/reports/~{}.md", secret.track_id.as_str()),
    ] {
        assert_refused(
            cat_blocks(&boot, planner(reader), &path, &[&secret_ids[1]]).await,
            INVALID_PARAMS,
            "no report at",
        );
    }
}

/// #1877: `sections` (`neige track cat <report> --sections`) prints whole H1 sections, byte-equal to
/// `neige_report_read { select: { sections } }` on every report path, with one shared refusal.
#[tokio::test]
async fn sections_text_equals_calm_report_read_select_on_every_report_path() {
    let boot = boot(&[(0, "认证 方案")]).await;
    let own = &boot.sides[0];
    write_body(
        &boot,
        own,
        &BODY.replace("# Next", "## Detail\n\ndelta\n\n# Next"),
    )
    .await;
    let sections = ["Next", "Findings"];
    let want = call(
        &boot,
        TOOL_REPORT_READ,
        planner(own),
        json!({ "select": { "sections": sections } }),
    )
    .await
    .expect("select.sections")["text"]
        .as_str()
        .expect("text")
        .to_string();
    let ids = outline_ids(&boot, own, own).await;
    assert_eq!(
        want,
        select_text(&boot, own, &[&ids[2], &ids[3], &ids[4]]).await,
        "a section is its H1 block and the blocks up to the next H1, in document order"
    );
    for path in ["report.md", "area/reports/认证 方案.md"] {
        let value = call(
            &boot,
            TOOL_TRACK_CAT,
            planner(own),
            json!({ "path": path, "sections": sections }),
        )
        .await
        .expect("cat sections");
        assert_eq!(value["content"].as_str(), Some(want.as_str()), "{path}");
        let err = call(
            &boot,
            TOOL_TRACK_CAT,
            planner(own),
            json!({ "path": path, "sections": ["Goal", "Nope"] }),
        )
        .await
        .expect_err("unknown section");
        assert_eq!(err.code, INVALID_PARAMS, "{path}: {err:?}");
        assert_eq!(
            err.message,
            "unknown section `Nope`; this report's sections are:\n  # Goal\n  # Findings\n  # Next",
            "{path}"
        );
    }
    assert_refused(
        call(
            &boot,
            TOOL_TRACK_CAT,
            planner(own),
            json!({ "path": "index.md", "sections": ["Goal"] }),
        )
        .await,
        INVALID_PARAMS,
        "is not a report; --sections reads sections of `report.md`",
    );
    assert_refused(
        call(
            &boot,
            TOOL_TRACK_CAT,
            planner(own),
            json!({ "path": "report.md", "sections": ["Goal"], "blocks": [&ids[1]] }),
        )
        .await,
        INVALID_PARAMS,
        "pass `blocks` or `sections`, not both",
    );
}
