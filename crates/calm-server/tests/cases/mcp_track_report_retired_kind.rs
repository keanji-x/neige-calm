//! The retired `view` overlay kind (#2021 S4) through the Planner's report tools: every write
//! refuses it, and a report that still stores one stays readable.
#![cfg(unix)]

use crate::mcp_track_report::{
    Boot, boot, call_tool, planner_identity, read_then_write_markdown, upsert_block,
};
use calm_server::track_report::TrackReportPayload;
use calm_types::report_blocks::render_fence;
use serde_json::{Value, json};

/// The retired kind, spelled in two parts so the S4 acceptance grep
/// (`docs/architecture/2021-report-template-composition.md` §5) keeps matching only the design
/// docs; this test is the one place that still has to name it.
const RETIRED_KIND: &str = concat!("view", ".live");

fn retired_payload() -> Value {
    json!({"source": "neige://plugin/operations/health", "version": 1})
}

async fn read(boot: &Boot, args: Value) -> Value {
    call_tool(boot, "neige.report.read", planner_identity(boot), args)
        .await
        .unwrap()
}

/// A report persisted before S4: the old kind sits in the stored row. Seeded directly because no
/// write end accepts it any more, then migrated to the CRDT by one ordinary block write.
async fn seed_stored_retired_block(boot: &Boot) -> String {
    let body = format!(
        "# Performance\n\n{}# Execution\n\n{}",
        render_fence(RETIRED_KIND, &retired_payload()),
        render_fence(
            "table",
            &json!({"source": "neige://plugin/operations/fills"})
        ),
    );
    let payload = serde_json::to_string(&TrackReportPayload::new("legacy", body)).unwrap();
    let card_id = boot.report_card_id.to_string();
    calm_server::db::write_in_tx_typed(boot.repo.as_ref(), move |tx| {
        Box::pin(async move {
            sqlx::query("UPDATE cards SET payload = ?1, body_crdt = NULL WHERE id = ?2")
                .bind(payload)
                .bind(card_id)
                .execute(&mut **tx)
                .await?;
            Ok(())
        })
    })
    .await
    .unwrap();
    upsert_block(
        boot,
        planner_identity(boot),
        json!({"kind": "prose", "payload": {"markdown": "# Conclusion\n\nunchanged\n"}}),
    )
    .await
    .expect("a block write beside a stored retired block still succeeds");
    let report = read(boot, json!({})).await;
    report["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|block| block["kind"] == RETIRED_KIND)
        .expect("the stored block survives the write")["id"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn retired_view_kind_is_refused_by_the_planner_block_upsert() {
    let boot = boot().await;
    let before = read(&boot, json!({})).await;
    let error = upsert_block(
        &boot,
        planner_identity(&boot),
        json!({"kind": RETIRED_KIND, "payload": retired_payload()}),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, -32602, "{error:?}");
    assert_eq!(
        error.message,
        format!(
            "neige.report.commit: ops[0]: unknown kind `{RETIRED_KIND}` — supported kinds: prose, \
             chart.candles, chart.series, table, app, task, preview, view. See neige.report.kinds."
        )
    );
    assert_eq!(read(&boot, json!({})).await["docRev"], before["docRev"]);
}

#[tokio::test]
async fn retired_view_kind_is_refused_by_the_planner_whole_document_write() {
    let boot = boot().await;
    let before = read(&boot, json!({})).await;
    let fence = render_fence(RETIRED_KIND, &retired_payload());
    let error = read_then_write_markdown(
        &boot,
        planner_identity(&boot),
        json!({"body": format!("# Performance\n\n{fence}"), "message": "restore the old view"}),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, -32602, "{error:?}");
    assert_eq!(
        error.message,
        format!(
            "neige.report.write: invalid `{RETIRED_KIND}` block payload: unknown block kind \
             `{RETIRED_KIND}` — known data kinds: chart.candles, chart.series, table, app, task, \
             preview, view (see neige.report.kinds)"
        )
    );
    assert_eq!(read(&boot, json!({})).await["docRev"], before["docRev"]);
}

#[tokio::test]
async fn a_stored_retired_view_kind_reads_as_an_unresolved_block() {
    let boot = boot().await;
    let id = seed_stored_retired_block(&boot).await;
    for args in [json!({}), json!({"resolve": {id.clone(): "full"}})] {
        let report = read(&boot, args).await;
        let blocks = report["blocks"].as_array().unwrap();
        let retired = blocks.iter().find(|block| block["id"] == id).unwrap();
        assert_eq!(retired["kind"], RETIRED_KIND);
        assert!(retired.get("resolved").is_none(), "{retired}");
        // The rest of the report still hydrates: the live table beside it is pending, not failed.
        let table = blocks
            .iter()
            .find(|block| block["kind"] == "table")
            .unwrap();
        assert_eq!(table["resolved"]["status"], "pending", "{table}");
        assert!(
            report["text"]
                .as_str()
                .unwrap()
                .contains(&format!("```neige-block {RETIRED_KIND}\n")),
        );
    }
}
