//! Global Report browsing. Absolute scopes never enter TrackFsView.
use super::{
    paging::{self, Page},
    track_file,
    track_report::{ReadSelect, parse_select_arg},
};
use crate::mcp_server::{framing::RpcError, registry::AppContext};
use crate::model::Track;
use serde_json::{Value, json};
use std::sync::Arc;

pub(crate) use crate::report_paths::Scope;
use crate::report_paths::user_area;

async fn target(ctx: &AppContext, area: &str, id: &str) -> Result<Track, RpcError> {
    user_area(ctx, area).await?;
    track_in_area(ctx, area, id).await
}

pub(super) async fn track_in_area(
    ctx: &AppContext,
    area: &str,
    id: &str,
) -> Result<Track, RpcError> {
    let track = ctx
        .repo
        .track_get(id)
        .await
        .map_err(internal)?
        .ok_or_else(|| RpcError::not_found("Track not found"))?;
    if track.area_id.as_str() != area {
        return Err(RpcError::forbidden("Track does not belong to Area"));
    }
    Ok(track)
}

pub(super) async fn snapshot(
    ctx: &Arc<AppContext>,
    track: &Track,
) -> Result<crate::track_report_read::ReportDocSnapshot, RpcError> {
    let (card, _) = super::track_report::load_report_for_track(ctx, track).await?;
    crate::track_report_read::load_report_doc_snapshot(ctx.repo.as_ref(), card.id.as_str())
        .await
        .map_err(internal)
}

pub(super) async fn report(
    ctx: &Arc<AppContext>,
    track: &Track,
    args: &Value,
) -> Result<Value, RpcError> {
    let selection = parse_select_arg(args, track_file::TOOL_CAT)?;
    let doc = snapshot(ctx, track).await?;
    match selection {
        ReadSelect::Full => Ok(content(doc.body, "text/markdown")),
        ReadSelect::Part(selection) => {
            let ids = selection.block_ids(&doc.blocks)?;
            track_file::report_blocks_content(&doc.blocks, &ids)
        }
        ReadSelect::Index => {
            let blocks: Vec<_> = doc.blocks.iter().map(|b| json!({"id":b.id,"kind":b.kind,"rev":b.rev,"heading":super::report_links::block_heading(b)})).collect();
            Ok(content(json!({"track_id":track.id,"area_id":track.area_id,"title":track.title,"summary":doc.summary,"doc_rev":doc.doc_rev,"blocks":blocks}).to_string(), "application/json"))
        }
    }
}

fn content(content: String, content_type: &str) -> Value {
    json!({"content":content,"content_type":content_type})
}
fn internal(e: impl std::fmt::Display) -> RpcError {
    RpcError::internal(e.to_string())
}
fn row(id: &str, title: &str, path: &str, kind: &str) -> Value {
    json!({"id":id,"title":title,"path":path,"kind":kind})
}

fn listing(mut rows: Vec<Value>, args: &Value, directory: &Value) -> Result<Value, RpcError> {
    rows.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    let cursor = paging::cursor_arg(args, track_file::TOOL_LS)?;
    if let Some(cursor) = cursor
        && !rows
            .iter()
            .any(|row| directory_cursor(directory, row["id"].as_str().unwrap()) == cursor)
    {
        return Err(paging::foreign_cursor(track_file::TOOL_LS, cursor));
    }
    let mut page = Page::new(50);
    let after = cursor
        .and_then(|cursor| {
            rows.iter()
                .find(|row| directory_cursor(directory, row["id"].as_str().unwrap()) == cursor)
        })
        .and_then(|row| row["id"].as_str());
    for row in rows
        .iter()
        .filter(|row| after.is_none_or(|after| row["id"].as_str().unwrap() > after))
    {
        if !page.push(
            directory_cursor(directory, row["id"].as_str().unwrap()),
            row.clone(),
        ) {
            break;
        }
    }
    let (entries, next_cursor) = page.finish(false);
    Ok(json!({"entries":entries,"next_cursor":next_cursor}))
}

fn directory_cursor(directory: &Value, id: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&(directory, id)).unwrap())
}

pub(super) async fn ls(
    ctx: &Arc<AppContext>,
    scope: Scope,
    args: &Value,
) -> Result<Value, RpcError> {
    let directory = match &scope {
        Scope::Root => json!(["root"]),
        Scope::System => json!(["system"]),
        Scope::Track(area, id) => json!(["track", area, id]),
        _ => Value::Null,
    };
    let rows = match scope {
        Scope::Root => {
            let mut rows = vec![row(".neige", ".neige", "/.neige/", "dir")];
            for (a, name) in crate::report_paths::areas(ctx).await? {
                let mut r = row(a.id.as_str(), &a.name, &format!("/{name}/"), "dir");
                r["area_id"] = json!(a.id);
                rows.push(r);
            }
            rows
        }
        Scope::System => vec![row(
            "report-changes",
            "Report changes",
            "/.neige/report-changes/",
            "dir",
        )],
        Scope::Area(area) => {
            user_area(ctx, &area).await?;
            let directory = json!(["area", area]);
            let root = crate::report_paths::area_path(ctx, &area).await?;
            let named = crate::report_paths::tracks(ctx, &area).await?;
            let paths: std::collections::HashMap<_, _> = named
                .iter()
                .map(|(t, n)| (t.id.to_string(), format!("{root}{n}/")))
                .collect();
            let mut tracks: Vec<_> = named.into_iter().map(|(t, _)| t).collect();
            tracks.retain(crate::track_access::user_visible_track);
            tracks.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
            let cursor = paging::cursor_arg(args, track_file::TOOL_LS)?;
            let after = cursor
                .map(|cursor| {
                    tracks
                        .iter()
                        .find(|track| directory_cursor(&directory, track.id.as_str()) == cursor)
                        .map(|track| track.id.as_str())
                        .ok_or_else(|| paging::foreign_cursor(track_file::TOOL_LS, cursor))
                })
                .transpose()?;
            let mut page = Page::new(50);
            for track in tracks
                .iter()
                .filter(|track| after.is_none_or(|after| track.id.as_str() > after))
            {
                let doc = snapshot(ctx, track).await?;
                let db = ctx
                    .sqlite_db
                    .as_ref()
                    .ok_or_else(|| internal("browse requires sqlite"))?;
                let count_sql = format!(
                    "SELECT count(*) FROM current_tasks WHERE track_id=?1 AND status IN {}",
                    crate::db::sqlite::RUNNING_WORK_STATUSES_SQL
                );
                let count: i64 = sqlx::query_scalar(&count_sql)
                    .bind(track.id.as_str())
                    .fetch_one(db.read_pool())
                    .await
                    .map_err(internal)?;
                let path = paths[track.id.as_str()].clone();
                let mut row = row(track.id.as_str(), &track.title, &path, "dir");
                let blocks: Vec<_> = doc.blocks.iter().take(40).map(|b| json!({"id":b.id,"kind":b.kind,"heading":super::report_links::block_heading(b)})).collect();
                row.as_object_mut().unwrap().extend(json!({"area_id":track.area_id,"track_id":track.id,"report_path":format!("{path}report.md"),"closed_at":crate::time_format::at_opt(track.closed_at),"creator_track_id":track.creator_track_id,"doc_rev":doc.doc_rev,"updated_at":crate::time_format::at(doc.updated_at),"active_tasks_count":count,"blocks":blocks,"blocks_truncated":doc.blocks.len().saturating_sub(40)}).as_object().unwrap().clone());
                if !page.push(directory_cursor(&directory, track.id.as_str()), row) {
                    break;
                }
            }
            let (entries, next_cursor) = page.finish(false);
            return Ok(json!({"entries":entries,"next_cursor":next_cursor}));
        }
        Scope::Track(area, id) => {
            target(ctx, &area, &id).await?;
            let root = crate::report_paths::track_path(ctx, &area, &id).await?;
            let mut rows = vec![
                row("report.md", "Report", &format!("{root}report.md"), "file"),
                row(
                    "report-edits",
                    "Report edits",
                    &format!("{root}report-edits/"),
                    "dir",
                ),
            ];
            for row in &mut rows {
                row["area_id"] = json!(area);
                row["track_id"] = json!(id);
            }
            rows
        }
        Scope::ChangesDir | Scope::EditsDir(..) => {
            if let Scope::EditsDir(ref area, ref id) = scope {
                track_in_area(ctx, area, id).await?;
            }
            if args.get("cursor").is_some() {
                return Err(RpcError::invalid_params("date directories have no cursor"));
            }
            return Ok(json!({"entries":[],"next_cursor":null,"date_pattern":"YYYY-MM-DD.json"}));
        }
        _ => return Err(RpcError::invalid_params("path is not a directory")),
    };
    listing(rows, args, &directory)
}

pub(super) async fn cat(
    ctx: &Arc<AppContext>,
    scope: Scope,
    args: &Value,
    zone: chrono_tz::Tz,
) -> Result<Value, RpcError> {
    match scope {
        Scope::Report(area, id) => {
            reject(args, &["step", "cursor", "through_event_id"])?;
            report(ctx, &target(ctx, &area, &id).await?, args).await
        }
        Scope::Changes(ref file) | Scope::Edits(_, _, ref file) => {
            reject(args, &["blocks", "sections", "detail", "step"])?;
            let date = file
                .strip_suffix(".json")
                .ok_or_else(|| RpcError::invalid_params("expected YYYY-MM-DD.json"))?;
            crate::workspace_reports::parse_date(date)
                .map_err(crate::mcp_server::framing::calm_error)?;
            let db = ctx
                .sqlite_db
                .as_ref()
                .ok_or_else(|| internal("browse requires sqlite"))?;
            let mut query = args.as_object().unwrap().clone();
            query.remove("path");
            query.insert("date".into(), json!(date));
            let value = if let Scope::Edits(ref area, ref id, _) = scope {
                track_in_area(ctx, area, id).await?;
                query.insert("track_id".into(), json!(id));
                let query = serde_json::from_value(Value::Object(query))
                    .map_err(|e| RpcError::invalid_params(e.to_string()))?;
                let page = crate::workspace_reports::edits(db.read_pool(), &query, zone)
                    .await
                    .map_err(crate::mcp_server::framing::calm_error)?;
                let edits: Vec<_> = page.edits.into_iter().map(|e| json!({"event_id":e.event_id,"edited_at":crate::time_format::at(e.at),"edit":e.edit})).collect();
                json!({"edits":edits,"next_cursor":page.next_cursor})
            } else {
                let query = serde_json::from_value(Value::Object(query))
                    .map_err(|e| RpcError::invalid_params(e.to_string()))?;
                serde_json::to_value(
                    crate::workspace_reports::changes(db, &query, zone)
                        .await
                        .map_err(crate::mcp_server::framing::calm_error)?,
                )
                .map_err(internal)?
            };
            Ok(content(value.to_string(), "application/json"))
        }
        _ => Err(RpcError::invalid_params(
            "path is not a readable Report file",
        )),
    }
}

pub(super) fn reject(args: &Value, keys: &[&str]) -> Result<(), RpcError> {
    if keys.iter().any(|key| args.get(key).is_some()) {
        return Err(RpcError::invalid_params(
            "options are not available for this path",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
