#![cfg(unix)]

use calm_server::mcp_server::tools::report_links::{TOOL_COVE_OUTLINE, TOOL_REPORT_BACKLINKS};
use calm_server::model::{NewCard, NewCove, NewWave};
use calm_server::wave_report::{WaveReportPayload, persist_report};
use calm_server::{event::EditAuthor, ids::ActorId};
use serde_json::{Value, json};

use crate::mcp_wave_report::{boot, call_tool, spec_identity, worker_identity};

async fn add_wave(
    boot: &crate::mcp_wave_report::Boot,
    cove_id: &str,
    title: &str,
    body: String,
) -> calm_server::model::Wave {
    let wave = boot
        .repo
        .wave_create(NewWave {
            cove_id: cove_id.into(),
            title: title.into(),
            sort: None,
            cwd: String::new(),
            workflow_id: None,
            plugin_scope: None,
            workflow_input: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let initial = WaveReportPayload::initial();
    let report = boot
        .repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "wave-report".into(),
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
        EditAuthor::Kernel,
        wave.clone(),
        report,
        initial,
        WaveReportPayload::new("", body),
        0,
        None,
        None,
        false,
    )
    .await
    .unwrap();
    wave
}

async fn strip_report_cache_and_crdt(
    boot: &crate::mcp_wave_report::Boot,
    wave: &calm_server::model::Wave,
) {
    sqlx::query(
        "UPDATE cards SET body_crdt = NULL, \
         payload = json_set(json_remove(payload, '$.blocks'), '$.schemaVersion', 1) \
         WHERE wave_id = ?1 AND kind = 'wave-report'",
    )
    .bind(wave.id.as_str())
    .execute(&boot.repo.sqlite_pool().unwrap())
    .await
    .unwrap();
}

#[tokio::test]
async fn outline_lists_same_cove_sibling_but_not_other_cove() {
    let boot = boot().await;
    let sibling = add_wave(
        &boot,
        boot.cove_id.as_str(),
        "Sibling",
        "# Sibling\n".into(),
    )
    .await;
    let other_cove = boot
        .repo
        .cove_create(NewCove {
            name: "other".into(),
            color: "#fff".into(),
            sort: None,
        })
        .await
        .unwrap();
    let outside = add_wave(
        &boot,
        other_cove.id.as_str(),
        "Outside",
        "# Outside\n".into(),
    )
    .await;
    let value = call_tool(&boot, TOOL_COVE_OUTLINE, spec_identity(&boot), json!({}))
        .await
        .unwrap();
    let waves = value["waves"].as_array().unwrap();
    assert!(waves.iter().any(|wave| wave["id"] == sibling.id.as_str()));
    assert!(!waves.iter().any(|wave| wave["id"] == outside.id.as_str()));
}

#[tokio::test]
async fn outline_derives_blocks_for_v1_report_without_crdt() {
    let boot = boot().await;
    let legacy = add_wave(
        &boot,
        boot.cove_id.as_str(),
        "Legacy",
        "# Legacy heading\n\nBody\n".into(),
    )
    .await;
    strip_report_cache_and_crdt(&boot, &legacy).await;

    let value = call_tool(&boot, TOOL_COVE_OUTLINE, spec_identity(&boot), json!({}))
        .await
        .unwrap();
    let wave = value["waves"]
        .as_array()
        .unwrap()
        .iter()
        .find(|wave| wave["id"] == legacy.id.as_str())
        .unwrap();
    assert_eq!(wave["blocks"][0]["heading"], "Legacy heading");
    assert!(wave["blocks"][0]["id"].as_str().unwrap().starts_with("b_"));
}

#[tokio::test]
async fn outline_wave_cap_is_reported_and_exact() {
    let boot = boot().await;
    for index in 0..50 {
        add_wave(
            &boot,
            boot.cove_id.as_str(),
            &format!("Sibling {index}"),
            String::new(),
        )
        .await;
    }

    let value = call_tool(&boot, TOOL_COVE_OUTLINE, spec_identity(&boot), json!({}))
        .await
        .unwrap();
    assert_eq!(value["waves"].as_array().unwrap().len(), 50);
    assert_eq!(value["truncated"]["waves"], 1);
}

/// A report that carries its own maintenance contract: one leading HTML
/// comment block (multi-line, spanning blank lines — a CommonMark HTML block of
/// type 2 does not end at one) plus four H1 sections. Built by hand on purpose:
/// in this slice `WaveReportPayload::initial()` is still the one-line
/// placeholder, so depending on it would measure nothing (#1185 S1).
const CONTRACT: &str = "<!-- 报告维护契约（渲染时被丢弃，读 body 源码的主体看得到）\n\
    \n\
    这份报告自带的结构就是规则：维护它，不要重写它。\n\
    \n\
    写作方式：散文正文控制在 1000 字以内，写产出，不写过程。\n\
    -->\n\n";

fn contract_body() -> String {
    format!(
        "{CONTRACT}# 概要\n\n本轮结论。\n\n# 待你定\n\n等你拍板的事。\n\n\
         # 已完成\n\n已成事实。\n\n# 决策\n\n定了的事。\n"
    )
}

#[tokio::test]
async fn outline_gives_a_contract_block_an_empty_heading_but_keeps_its_id() {
    // #1185 §5.8 — the contract renders as nothing, so it may not become a
    // block title; but the entry stays, because this outline is the only
    // source of block ids for deep links.
    let boot = boot().await;
    let wave = add_wave(&boot, boot.cove_id.as_str(), "Carrier", contract_body()).await;

    let value = call_tool(&boot, TOOL_COVE_OUTLINE, spec_identity(&boot), json!({}))
        .await
        .unwrap();
    let entry = value["waves"]
        .as_array()
        .unwrap()
        .iter()
        .find(|candidate| candidate["id"] == wave.id.as_str())
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
async fn outline_of_a_cove_full_of_contract_bearing_reports_has_headroom_under_the_caps() {
    /*
     * What this measures, exactly: a cove at the realistic ceiling — 51 waves,
     * every one carrying the maintenance contract plus four sections — still
     * fits the outline response comfortably, and the only degradation is the
     * wave cap, reported.
     *
     * What it does NOT measure, so nobody reads it as coverage it lacks: the
     * `MAX_RESPONSE_BYTES` truncation branch and the `MAX_BLOCKS_PER_WAVE`
     * branch are both untaken here, and by construction. 51 waves × 5 blocks
     * serializes to about 17 KB against a 32 KiB cap, and 5 is well under the
     * 40-block cap. The assertions below therefore say "nothing was dropped",
     * which is the claim worth pinning for the carrier: adding a contract block
     * to every report does not cost a cove its outline. A test of the *drop*
     * paths would need a fixture built to blow the caps, it would be about the
     * degradation logic rather than about the contract, and it is not this one.
     *
     * The existing wave-cap test seeds 50 *empty* bodies, which is why the size
     * question needed a fixture of its own (#1185 §4.4 E).
     */
    let boot = boot().await;
    let mut seeded = Vec::new();
    for index in 0..50 {
        seeded.push(
            add_wave(
                &boot,
                boot.cove_id.as_str(),
                &format!("Sibling {index}"),
                contract_body(),
            )
            .await,
        );
    }

    let value = call_tool(&boot, TOOL_COVE_OUTLINE, spec_identity(&boot), json!({}))
        .await
        .unwrap();
    let bytes = serde_json::to_vec(&value).unwrap().len();
    // Measured at 17029 bytes when this was written. The upper bound is the cap
    // itself; the lower bound is there so the day someone breaks the fixture
    // into emptiness this stops passing for the wrong reason.
    assert!(
        (10 * 1024..=32 * 1024).contains(&bytes),
        "expected a real, capped payload for 51 contract-bearing waves; got {bytes} bytes"
    );

    // The wave cap IS taken here — 51 waves against `MAX_WAVES = 50` — and it
    // is reported rather than silent.
    let waves = value["waves"].as_array().unwrap();
    assert_eq!(waves.len(), 50);
    assert_eq!(value["truncated"]["waves"], 1);
    // Stated rather than implied: the byte-truncation branch did not fire.
    assert!(value["truncated"]["bytes"].is_null());

    // No block was dropped from ANY wave — not just from the ones this loop
    // happens to recognise. `truncated.blocks` is omitted entirely when the map
    // is empty, so asserting the whole key absent also rules out truncation
    // metadata parked under some other wave id.
    assert!(
        value["truncated"]["blocks"].is_null(),
        "nothing was dropped, so `truncated.blocks` must be absent entirely; got {}",
        value["truncated"]["blocks"]
    );

    // Every block of every seeded wave is listed. This is the carrier's actual
    // claim — a contract block in every report costs the cove nothing in
    // outline coverage — so the loop must also prove it actually looked at the
    // seeded waves: a fixture whose ids stopped matching would otherwise skip
    // every iteration and still pass.
    let mut verified = 0usize;
    let mut foreign = Vec::new();
    for wave in waves {
        let id = wave["id"].as_str().unwrap();
        if !seeded.iter().any(|seed| seed.id.as_str() == id) {
            foreign.push(id.to_string());
            continue;
        }
        verified += 1;
        assert_eq!(
            wave["blocks"].as_array().unwrap().len(),
            5,
            "every block of a seeded wave is listed ({id})"
        );
    }
    // The cove holds 51 waves — 50 seeded plus the boot wave — and `MAX_WAVES`
    // lists the 50 lowest ids, so exactly one falls off, and which one depends
    // on where the random ids sort. Hence: nothing but the boot wave may show
    // up unrecognised, and at most one seeded wave may be missing.
    assert!(
        (seeded.len() - 1..=seeded.len()).contains(&verified),
        "expected to have checked all {} seeded waves (at most one displaced by the wave cap); \
         checked {verified}",
        seeded.len()
    );
    assert!(
        foreign
            .iter()
            .all(|id| id.as_str() == boot.wave_id.as_str()),
        "the only listable wave this test did not seed is the boot wave; got {foreign:?}"
    );
}

#[tokio::test]
async fn backlinks_returns_linking_wave_for_callers_wave() {
    let boot = boot().await;
    let source = add_wave(
        &boot,
        boot.cove_id.as_str(),
        "Source",
        format!("[target](neige://wave/{})\n", boot.wave_id),
    )
    .await;

    let value = call_tool(
        &boot,
        TOOL_REPORT_BACKLINKS,
        spec_identity(&boot),
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(value["backlinks"][0]["src_wave_id"], source.id.as_str());
    assert_eq!(value["backlinks"][0]["label"], "target");
}

#[tokio::test]
async fn backlinks_returns_link_from_footnote_definition_with_stable_shape() {
    let boot = boot().await;
    let source = add_wave(
        &boot,
        boot.cove_id.as_str(),
        "Footnote source",
        format!("[^note]: [footnote](neige://wave/{})\n", boot.wave_id),
    )
    .await;

    let value = call_tool(
        &boot,
        TOOL_REPORT_BACKLINKS,
        spec_identity(&boot),
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
                "src_wave_id": source.id,
                "src_wave_title": "Footnote source",
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
async fn report_link_reads_reject_non_spec_caller() {
    let boot = boot().await;
    for tool in [TOOL_COVE_OUTLINE, TOOL_REPORT_BACKLINKS] {
        let error = call_tool(
            &boot,
            tool,
            worker_identity(&boot),
            Value::Object(Default::default()),
        )
        .await
        .unwrap_err();
        assert!(error.message.contains("requires role=Spec"));
    }
}
