//! Immutable, paged navigation evidence; never a read of current report text.
use super::*;
use crate::mcp_server::tools::paging::{self, Page};

pub(super) async fn read(
    ctx: &Arc<AppContext>,
    identity: ToolCallIdentity,
    args: &Value,
) -> Result<Option<Value>, RpcError> {
    let Some(commit) = args.get("commit") else {
        if args.get("cursor").is_some() {
            return Err(RpcError::invalid_params(
                "cursor requires commit and detail:index",
            ));
        }
        return Ok(None);
    };
    let commit = commit
        .as_str()
        .filter(|c| !c.is_empty())
        .ok_or_else(|| RpcError::invalid_params("commit must be a nonempty history reference"))?
        .to_string();
    if args["detail"] != "index"
        || ["blocks", "sections", "resolve", "with_markers"]
            .iter()
            .any(|key| args.get(*key).is_some())
    {
        return Err(RpcError::invalid_params(
            "commit requires detail:index without text or hydration selectors",
        ));
    }
    let cursor = paging::cursor_arg(args, TOOL_REPORT_READ)?.map(str::to_owned);
    let requested = commit.clone();
    let payload = crate::db::write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
        let identity = identity.clone();
        let commit = requested.clone();
        Box::pin(async move {
            let target = crate::track_report::identity::resolve_tx(
                tx,
                &identity,
                crate::track_report::identity::Access::Read,
            )
            .await?;
            let (track, report, _) = target.into_parts();
            crate::report_read_ledger::history::payload_at_tx(
                tx, &track.id, report.id.as_str(), &commit,
            )
            .await?
            .ok_or_else(|| {
                crate::error::CalmError::NotFound(
                    "Report history is unavailable in this Track (unknown, pruned or invalid snapshot)".into(),
                )
            })
        })
    })
    .await
    .map_err(crate::mcp_server::framing::calm_error)?;
    let blocks = payload
        .blocks
        .ok_or_else(|| RpcError::internal("Report history has no block snapshot"))?;
    let start = match cursor.as_deref() {
        None => 0,
        Some(cursor) => blocks
            .iter()
            .position(|block| format!("{commit}:{}", block.id) == cursor)
            .map(|position| position + 1)
            .ok_or_else(|| paging::foreign_cursor(TOOL_REPORT_READ, cursor))?,
    };
    let mut page = Page::with_budget(50, 8 * 1024 - 2);
    for block in &blocks[start..] {
        if !page.push(
            format!("{commit}:{}", block.id),
            json!({"id":block.id,"kind":block.kind,"rev":block.rev}),
        ) {
            break;
        }
    }
    let (rows, next_cursor) = page.finish(false);
    Ok(Some(json!({
        "commit":commit, "doc_rev":payload.doc_rev, "schema_version":payload.schema_version,
        "blocks_total":blocks.len(), "blocks":rows,
        "complete":next_cursor.is_null(), "next_cursor":next_cursor,
        "knowledge_granted":false,
    })))
}
