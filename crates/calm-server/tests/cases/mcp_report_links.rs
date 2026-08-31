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
async fn outline_stays_within_the_response_byte_cap_with_multi_block_reports() {
    // The existing wave-cap test seeds 50 *empty* bodies, so it exercises
    // neither `MAX_BLOCKS_PER_WAVE` nor `MAX_RESPONSE_BYTES` (#1185 §4.4 E).
    // Five blocks per wave times 50 waves is the shape a document-carried
    // contract actually produces.
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
    // Measured at 17029 bytes for these 51 waves — real headroom under the cap,
    // not an accident of an empty fixture.
    assert!(
        bytes <= 32 * 1024,
        "cove.outline must honour MAX_RESPONSE_BYTES; got {bytes} bytes"
    );

    // Degradation is reported, never silent.
    let waves = value["waves"].as_array().unwrap();
    let listed_waves = waves.len();
    let omitted_waves = value["truncated"]["waves"].as_u64().unwrap_or(0) as usize;
    // 50 siblings + the wave `boot()` itself created.
    assert_eq!(listed_waves + omitted_waves, 51);

    for wave in waves {
        let id = wave["id"].as_str().unwrap();
        if !seeded.iter().any(|seed| seed.id.as_str() == id) {
            continue;
        }
        let listed = wave["blocks"].as_array().unwrap().len();
        let omitted = value["truncated"]["blocks"][id].as_u64().unwrap_or(0) as usize;
        assert_eq!(
            listed + omitted,
            5,
            "a seeded wave has five blocks; each is listed or reported truncated ({id})"
        );
    }
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
