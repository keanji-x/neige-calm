#![cfg(unix)]

use calm_server::mcp_server::tools::report_links::{TOOL_AREA_OUTLINE, TOOL_REPORT_BACKLINKS};
use calm_server::model::{NewArea, NewCard, NewTrack};
use calm_server::track_report::{TrackReportPayload, persist_report};
use calm_server::{event::EditAuthor, ids::ActorId};
use calm_types::report_blocks::render_fence;
use serde_json::{Value, json};

use crate::mcp_track_report::{boot, call_tool, planner_identity, worker_identity};

async fn add_track(
    boot: &crate::mcp_track_report::Boot,
    area_id: &str,
    title: &str,
    body: String,
) -> calm_server::model::Track {
    add_track_as(boot, area_id, title, body, EditAuthor::Kernel).await
}

async fn add_track_as(
    boot: &crate::mcp_track_report::Boot,
    area_id: &str,
    title: &str,
    body: String,
    author: EditAuthor,
) -> calm_server::model::Track {
    let track = boot
        .repo
        .track_create(NewTrack {
            area_id: area_id.into(),
            title: title.into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let initial = TrackReportPayload::initial();
    let report = boot
        .repo
        .card_create(NewCard {
            track_id: track.id.clone(),
            kind: "track-report".into(),
            sort: Some(-1.0),
            payload: serde_json::to_value(&initial).unwrap(),
            title: None,
        })
        .await
        .unwrap();
    persist_report(
        boot.repo.as_ref(),
        &boot.ctx.events,
        &boot.ctx.write,
        ActorId::Kernel,
        author,
        track.clone(),
        report,
        initial,
        TrackReportPayload::new("", body),
        0,
        None,
        None,
        false,
    )
    .await
    .unwrap();
    track
}

async fn strip_report_cache_and_crdt(
    boot: &crate::mcp_track_report::Boot,
    track: &calm_server::model::Track,
) {
    sqlx::query(
        "UPDATE cards SET body_crdt = NULL, \
         payload = json_set(json_remove(payload, '$.blocks'), '$.schemaVersion', 1) \
         WHERE track_id = ?1 AND kind = 'track-report'",
    )
    .bind(track.id.as_str())
    .execute(&boot.repo.sqlite_pool().unwrap())
    .await
    .unwrap();
}

#[tokio::test]
async fn outline_lists_same_area_sibling_but_not_other_area() {
    let boot = boot().await;
    let sibling = add_track(&boot, boot.area_id.as_str(), "Sibling", String::new()).await;
    let other_area = boot
        .repo
        .area_create(NewArea {
            name: "other".into(),
            color: "#fff".into(),
            sort: None,
        })
        .await
        .unwrap();
    let outside = add_track(
        &boot,
        other_area.id.as_str(),
        "Outside",
        "# Outside\n".into(),
    )
    .await;
    let value = call_tool(&boot, TOOL_AREA_OUTLINE, planner_identity(&boot), json!({}))
        .await
        .unwrap();
    let tracks = value["tracks"].as_array().unwrap();
    let sibling = tracks
        .iter()
        .find(|track| track["id"] == sibling.id.as_str())
        .expect("an empty same-area report is still listed");
    let blocks = sibling["blocks"].as_array().expect("outline blocks");
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0]["heading"], "");
    assert_eq!(blocks[0]["kind"], "prose");
    assert!(
        !tracks
            .iter()
            .any(|track| track["id"] == outside.id.as_str())
    );
}

#[tokio::test]
async fn outline_derives_blocks_for_v1_report_without_crdt() {
    let boot = boot().await;
    let legacy = add_track(
        &boot,
        boot.area_id.as_str(),
        "Legacy",
        "# Legacy heading\n\nBody\n".into(),
    )
    .await;
    strip_report_cache_and_crdt(&boot, &legacy).await;

    let value = call_tool(&boot, TOOL_AREA_OUTLINE, planner_identity(&boot), json!({}))
        .await
        .unwrap();
    let track = value["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|track| track["id"] == legacy.id.as_str())
        .unwrap();
    assert_eq!(track["blocks"][0]["heading"], "Legacy heading");
    assert!(track["blocks"][0]["id"].as_str().unwrap().starts_with("b_"));
}

#[tokio::test]
async fn outline_labels_live_and_tombstone_tasks_at_the_mcp_boundary() {
    let boot = boot().await;
    let live = json!({
        "key": "ship-heading",
        "kind": "codex",
        "goal": "[Ship task headings](https://example.com/raw-task-goal)",
        "ready": true,
        "declared_by": "user"
    });
    let tombstone = json!({
        "key": "retired-heading",
        "tombstone": { "reason": "Not needed" },
        "declared_by": "user",
        "tombstoned_by": "user"
    });
    let chart = json!({
        "symbol": "KEEP.US",
        "candles": [
            [1719800000000_i64, 10, 12, 9, 11, 100],
            [1719886400000_i64, 11, 13, 10, 12, 120]
        ]
    });
    let body = format!(
        "# Tasks\n\n{}{}{}",
        render_fence("task", &live),
        render_fence("task", &tombstone),
        render_fence("chart.candles", &chart),
    );
    let task_track = add_track_as(
        &boot,
        boot.area_id.as_str(),
        "Tasks",
        body,
        EditAuthor::User,
    )
    .await;

    let value = call_tool(&boot, TOOL_AREA_OUTLINE, planner_identity(&boot), json!({}))
        .await
        .unwrap();
    let blocks = value["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|track| track["id"] == task_track.id.as_str())
        .unwrap()["blocks"]
        .as_array()
        .unwrap();

    assert_eq!(blocks.len(), 4);
    assert_eq!(blocks[0]["heading"], "Tasks");
    assert_eq!(blocks[1]["heading"], "task: goal=Ship task headings");
    assert_eq!(blocks[2]["heading"], "task: key=retired-heading");
    assert_eq!(blocks[3]["heading"], "chart.candles: symbol=KEEP.US");
}

/// The real skeleton every track is born with — the kernel's own bytes, not a
/// transcription (#1185 §4.4 E). Since S2 that is one leading HTML comment
/// block (multi-line, spanning blank lines — a CommonMark HTML block of type 2
/// does not end at one) plus the four H1 sections the comment declares, so
/// depending on it measures exactly what ships. A hand-written stand-in would
/// only prove the outline can handle *a* contract-bearing report.
fn contract_body() -> String {
    TrackReportPayload::initial().body
}

#[tokio::test]
async fn outline_gives_a_contract_block_an_empty_heading_but_keeps_its_id() {
    // #1185 §5.8 — the contract renders as nothing, so it may not become a
    // block title; but the entry stays, because this outline is the only
    // source of block ids for deep links.
    let boot = boot().await;
    let track = add_track(&boot, boot.area_id.as_str(), "Carrier", contract_body()).await;

    let value = call_tool(&boot, TOOL_AREA_OUTLINE, planner_identity(&boot), json!({}))
        .await
        .unwrap();
    let entry = value["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|candidate| candidate["id"] == track.id.as_str())
        .unwrap();
    let blocks = entry["blocks"].as_array().unwrap();
    assert_eq!(
        blocks.len(),
        5,
        "contract block + four sections: {blocks:#?}"
    );
    assert_eq!(blocks[0]["heading"], "");
    assert!(blocks[0]["id"].as_str().unwrap().starts_with("b_"));
    assert_eq!(blocks[0]["kind"], "prose");
    assert_eq!(blocks[1]["heading"], "概要");
    assert_eq!(blocks[4]["heading"], "决策");
    // Nothing of the contract survives anywhere in the response.
    let serialized = serde_json::to_string(&value).unwrap();
    assert!(!serialized.contains("报告维护契约"));
    assert!(!serialized.contains("散文正文"));
}

#[tokio::test]
async fn outline_of_a_area_full_of_contract_bearing_reports_has_headroom_under_the_caps() {
    /*
     * What this measures, exactly: an area at the realistic ceiling — 51 tracks,
     * every one carrying the maintenance contract plus four sections — still
     * fits the outline response comfortably, and the only degradation is the
     * track cap, reported.
     *
     * What it does NOT measure, so nobody reads it as coverage it lacks: the
     * `MAX_RESPONSE_BYTES` truncation branch and the `MAX_BLOCKS_PER_TRACK`
     * branch are both untaken here, and by construction. 51 tracks × 5 blocks
     * serializes to about 17 KB against a 32 KiB cap, and 5 is well under the
     * 40-block cap. The assertions below therefore say "nothing was dropped",
     * which is the claim worth pinning for the carrier: adding a contract block
     * to every report does not cost an area its outline. A test of the *drop*
     * paths would need a fixture built to blow the caps, it would be about the
     * degradation logic rather than about the contract, and it is not this one.
     *
     * Empty-report inclusion is pinned by the cheap one-track case above. This
     * fixture owns the expensive 50-track cap assertion as well as realistic
     * payload headroom, so the suite does not seed the same ceiling twice.
     */
    let boot = boot().await;
    let mut seeded = Vec::new();
    for index in 0..50 {
        seeded.push(
            add_track(
                &boot,
                boot.area_id.as_str(),
                &format!("Sibling {index}"),
                contract_body(),
            )
            .await,
        );
    }

    let value = call_tool(&boot, TOOL_AREA_OUTLINE, planner_identity(&boot), json!({}))
        .await
        .unwrap();
    let bytes = serde_json::to_vec(&value).unwrap().len();
    // Measured at 17029 bytes when this was written. The upper bound is the cap
    // itself; the lower bound is there so the day someone breaks the fixture
    // into emptiness this stops passing for the wrong reason.
    assert!(
        (10 * 1024..=32 * 1024).contains(&bytes),
        "expected a real, capped payload for 51 contract-bearing tracks; got {bytes} bytes"
    );

    // The track cap IS taken here — 51 tracks against `MAX_TRACKS = 50` — and it
    // is reported rather than silent.
    let tracks = value["tracks"].as_array().unwrap();
    assert_eq!(tracks.len(), 50);
    assert_eq!(value["truncated"]["tracks"], 1);
    // Stated rather than implied: the byte-truncation branch did not fire.
    assert!(value["truncated"]["bytes"].is_null());

    // No block was dropped from ANY track — not just from the ones this loop
    // happens to recognise. `truncated.blocks` is omitted entirely when the map
    // is empty, so asserting the whole key absent also rules out truncation
    // metadata parked under some other track id.
    assert!(
        value["truncated"]["blocks"].is_null(),
        "nothing was dropped, so `truncated.blocks` must be absent entirely; got {}",
        value["truncated"]["blocks"]
    );

    // Every block of every seeded track is listed. This is the carrier's actual
    // claim — a contract block in every report costs the area nothing in
    // outline coverage — so the loop must also prove it actually looked at the
    // seeded tracks: a fixture whose ids stopped matching would otherwise skip
    // every iteration and still pass.
    let mut verified = 0usize;
    let mut foreign = Vec::new();
    for track in tracks {
        let id = track["id"].as_str().unwrap();
        if !seeded.iter().any(|seed| seed.id.as_str() == id) {
            foreign.push(id.to_string());
            continue;
        }
        verified += 1;
        assert_eq!(
            track["blocks"].as_array().unwrap().len(),
            5,
            "every block of a seeded track is listed ({id})"
        );
    }
    // The area holds 51 tracks — 50 seeded plus the boot track — and `MAX_TRACKS`
    // lists the 50 lowest ids, so exactly one falls off, and which one depends
    // on where the random ids sort. Hence: nothing but the boot track may show
    // up unrecognised, and at most one seeded track may be missing.
    assert!(
        (seeded.len() - 1..=seeded.len()).contains(&verified),
        "expected to have checked all {} seeded tracks (at most one displaced by the track cap); \
         checked {verified}",
        seeded.len()
    );
    assert!(
        foreign
            .iter()
            .all(|id| id.as_str() == boot.track_id.as_str()),
        "the only listable track this test did not seed is the boot track; got {foreign:?}"
    );
}

#[tokio::test]
async fn backlinks_returns_linking_track_for_callers_track() {
    let boot = boot().await;
    let source = add_track(
        &boot,
        boot.area_id.as_str(),
        "Source",
        format!("[target](neige://wave/{})\n", boot.track_id),
    )
    .await;

    let value = call_tool(
        &boot,
        TOOL_REPORT_BACKLINKS,
        planner_identity(&boot),
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(value["backlinks"][0]["src_track_id"], source.id.as_str());
    assert_eq!(value["backlinks"][0]["label"], "target");
}

#[tokio::test]
async fn backlinks_returns_link_from_footnote_definition_with_stable_shape() {
    let boot = boot().await;
    let source = add_track(
        &boot,
        boot.area_id.as_str(),
        "Footnote source",
        format!("[^note]: [footnote](neige://wave/{})\n", boot.track_id),
    )
    .await;

    let value = call_tool(
        &boot,
        TOOL_REPORT_BACKLINKS,
        planner_identity(&boot),
        json!({}),
    )
    .await
    .unwrap();
    let backlink = &value["backlinks"][0];
    let src_block_id = backlink["src_block_id"].as_str().unwrap();
    let updated_at = backlink["updated_at"].as_i64().unwrap();
    assert_eq!(
        value,
        json!({
            "backlinks": [{
                "src_track_id": source.id,
                "src_track_title": "Footnote source",
                "src_block_id": src_block_id,
                "dst_block_id": null,
                "label": "footnote",
                "updated_at": updated_at,
            }],
            "truncated": false,
            "skipped_sources": 0,
        })
    );
}

#[tokio::test]
async fn report_link_reads_reject_non_planner_caller() {
    let boot = boot().await;
    for tool in [TOOL_AREA_OUTLINE, TOOL_REPORT_BACKLINKS] {
        let error = call_tool(
            &boot,
            tool,
            worker_identity(&boot),
            Value::Object(Default::default()),
        )
        .await
        .unwrap_err();
        assert!(error.message.contains("requires role=Planner"));
    }
}
