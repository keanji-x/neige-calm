//! Bounded pair operations owned by the existing report transaction.
use super::{ReportDoc, ReportDocOp};
use crate::{
    error::{CalmError, Result},
    file_delivery::repair::{Derived, Receipt},
    operation::Tx,
};
use serde_json::{Value, json};
pub(super) fn prepare(doc: &ReportDoc, derived: &Derived) -> Result<ReportDocOp> {
    Ok(ReportDocOp::UpsertBlock {
        id: None,
        kind: calm_types::report_blocks::KIND_TASK.into(),
        content: calm_types::report_blocks::render_data_block(
            calm_types::report_blocks::KIND_TASK,
            &derived.payload,
        )
        .map_err(CalmError::BadRequest)?,
        if_rev: None,
        if_doc_rev: Some(
            doc.doc_rev()
                .map_err(|e| CalmError::Internal(e.to_string()))?,
        ),
        position: None,
    })
}
pub(super) async fn snapshot_tx(
    tx: &mut Tx<'_>,
    receipt: &Receipt,
    fallback: i64,
) -> Result<Value> {
    let (_, blocks) = super::report_blocks_snapshot_tx(tx, &receipt.track_id).await?;
    let (declarations, diagnostics) =
        calm_types::report_blocks::tasks::project_task_declarations(&blocks);
    let configured: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key=?1")
        .bind(crate::routes::settings::TASK_BUDGET_DEFAULT_KEY)
        .fetch_optional(&mut **tx)
        .await?;
    let budget =
        crate::routes::settings::effective_task_budget_default(configured.as_deref(), fallback);
    let verdicts = crate::db::sqlite::evaluate_schedulability_with_task_budget_default(
        tx,
        &receipt.track_id,
        &declarations,
        &diagnostics,
        budget,
    )
    .await?;
    let track = crate::track_lifecycle::track_get_tx(tx, &receipt.track_id.clone().into()).await?;
    let mut current = Vec::new();
    for derived in [&receipt.repair, &receipt.reviewer] {
        let declaration = declarations
            .iter()
            .find(|d| d.key == derived.key && d.block_id == derived.block_id);
        let exact = blocks.iter().any(|b| {
            b.id == derived.block_id
                && calm_types::task_recovery::task_root_hash_preimage(&b.payload)
                    == calm_types::task_recovery::task_root_hash_preimage(&derived.payload)
        });
        let allocation =
            crate::db::sqlite::task_attempt_current_tx(tx, &receipt.track_id, &derived.key).await?;
        let task = if let Some(a) = &allocation {
            crate::db::sqlite::task_get_tx(tx, &a.attempt_id).await?
        } else {
            None
        };
        let blocking = if let Some(a) = &allocation {
            crate::task_recovery::current_blocking_reason_tx(tx, &track, a, task.as_ref(), budget)
                .await?
        } else {
            None
        };
        current.push(json!({"key":derived.key,"declaration_present":declaration.is_some(),
            "declaration_withdrawn":declaration.is_some_and(|d|d.tombstone),
            "contract_status":if exact {"matches_repair"} else {"unavailable_or_changed"},
            "diagnostics":verdicts.iter().filter(|v|v.key == derived.key).collect::<Vec<_>>(),
            "blocking_reason":blocking,"allocation":allocation,
            "task":task.map(|t|json!({"attempt_id":t.id,"status":t.status,"status_detail":t.status_detail}))}));
    }
    Ok(
        json!({"receipt":receipt.public(),"stage":crate::file_delivery::repair_view::view_tx(tx, receipt).await?,"current":{"as_of_ms":crate::model::now_ms(),"lifecycle":track.lifecycle,"tasks":current}}),
    )
}
