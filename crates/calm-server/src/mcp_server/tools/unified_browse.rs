//! Global Report browsing. Absolute scopes never enter TrackFsView.
use super::{
    paging::{self, Page},
    track_file,
    track_report::{ReadSelect, parse_select_arg},
};
use crate::mcp_server::{framing::RpcError, registry::AppContext};
use crate::model::{AreaKind, Track};
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Debug, Clone, Copy)]
pub(super) enum Scope<'a> {
    Bound(&'a str),
    OwnReport,
    Legacy(crate::area_reports::AreaPath<'a>),
    Root,
    Areas,
    Area(&'a str),
    Tracks(&'a str),
    Track(&'a str, &'a str),
    Report(&'a str, &'a str),
    ChangesDir,
    Changes(&'a str),
    EditsDir(&'a str, &'a str),
    Edits(&'a str, &'a str, &'a str),
}

pub(super) fn resolve(path: &str) -> Result<Scope<'_>, RpcError> {
    let bad = || RpcError::invalid_params("path not available in this view");
    if path.split('/').any(|part| part == ".." || part == ".") {
        return Err(bad());
    }
    if !path.starts_with('/') {
        let path = path.trim_end_matches('/');
        if path == "report.md" {
            return Ok(Scope::OwnReport);
        }
        if let Some(path) = crate::area_reports::classify(path) {
            return path.map(Scope::Legacy).map_err(RpcError::invalid_params);
        }
        return Ok(Scope::Bound(path));
    }
    let parts: Vec<_> = path.trim_end_matches('/').split('/').skip(1).collect();
    Ok(match parts.as_slice() {
        [""] | [] => Scope::Root,
        ["areas"] => Scope::Areas,
        ["areas", area] if !area.is_empty() => Scope::Area(area),
        ["areas", area, "tracks"] => Scope::Tracks(area),
        ["areas", area, "tracks", track] => Scope::Track(area, track),
        ["areas", area, "tracks", track, file] => match *file {
            "report.md" => Scope::Report(area, track),
            "report-edits" => Scope::EditsDir(area, track),
            _ => return Err(bad()),
        },
        ["report-changes"] => Scope::ChangesDir,
        ["report-changes", date] => Scope::Changes(date),
        ["areas", area, "tracks", track, "report-edits", date] => Scope::Edits(area, track, date),
        _ => return Err(bad()),
    })
}

pub(super) async fn is_user_area(ctx: &AppContext, area: &str) -> Result<bool, RpcError> {
    let area = ctx
        .repo
        .area_get(area)
        .await
        .map_err(internal)?
        .ok_or_else(|| RpcError::not_found("Area not found"))?;
    Ok(area.kind == AreaKind::User)
}

pub(super) async fn user_area(ctx: &AppContext, area: &str) -> Result<(), RpcError> {
    if !is_user_area(ctx, area).await? {
        return Err(RpcError::forbidden("Report is outside user Areas"));
    }
    Ok(())
}

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

fn listing(mut rows: Vec<Value>, args: &Value) -> Result<Value, RpcError> {
    rows.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    let cursor = paging::cursor_arg(args, track_file::TOOL_LS)?;
    if let Some(cursor) = cursor
        && !rows.iter().any(|row| row["path"] == cursor)
    {
        return Err(paging::foreign_cursor(track_file::TOOL_LS, cursor));
    }
    let mut page = Page::new(50);
    let after = cursor
        .and_then(|cursor| rows.iter().find(|row| row["path"] == cursor))
        .and_then(|row| row["id"].as_str());
    for row in rows
        .iter()
        .filter(|row| after.is_none_or(|after| row["id"].as_str().unwrap() > after))
    {
        if !page.push(row["path"].as_str().unwrap().to_string(), row.clone()) {
            break;
        }
    }
    let (entries, next_cursor) = page.finish(false);
    Ok(json!({"entries":entries,"next_cursor":next_cursor}))
}

pub(super) async fn ls(
    ctx: &Arc<AppContext>,
    scope: Scope<'_>,
    args: &Value,
) -> Result<Value, RpcError> {
    let rows = match scope {
        Scope::Root => vec![
            row("areas", "Areas", "/areas/", "dir"),
            row(
                "report-changes",
                "Report changes",
                "/report-changes/",
                "dir",
            ),
        ],
        Scope::Areas => ctx
            .repo
            .areas_list_user_visible()
            .await
            .map_err(internal)?
            .into_iter()
            .map(|a| {
                let mut row = row(a.id.as_str(), &a.name, &format!("/areas/{}/", a.id), "dir");
                row["area_id"] = json!(a.id);
                row
            })
            .collect(),
        Scope::Area(area) => {
            user_area(ctx, area).await?;
            vec![row(
                "tracks",
                "Tracks",
                &format!("/areas/{area}/tracks/"),
                "dir",
            )]
        }
        Scope::Tracks(area) => {
            user_area(ctx, area).await?;
            let mut tracks = ctx.repo.tracks_by_area(area).await.map_err(internal)?;
            tracks.retain(crate::track_access::user_visible_track);
            tracks.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
            let cursor = paging::cursor_arg(args, track_file::TOOL_LS)?;
            let after = cursor
                .map(|cursor| {
                    tracks
                        .iter()
                        .find(|track| format!("/areas/{area}/tracks/{}/", track.id) == cursor)
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
                let path = format!("/areas/{area}/tracks/{}/", track.id);
                let mut row = row(track.id.as_str(), &track.title, &path, "dir");
                let blocks: Vec<_> = doc.blocks.iter().take(40).map(|b| json!({"id":b.id,"kind":b.kind,"heading":super::report_links::block_heading(b)})).collect();
                row.as_object_mut().unwrap().extend(json!({"area_id":track.area_id,"track_id":track.id,"report_path":format!("{path}report.md"),"closed_at":crate::time_format::at_opt(track.closed_at),"creator_track_id":track.creator_track_id,"doc_rev":doc.doc_rev,"updated_at":crate::time_format::at(doc.updated_at),"active_tasks_count":count,"blocks":blocks,"blocks_truncated":doc.blocks.len().saturating_sub(40)}).as_object().unwrap().clone());
                if !page.push(path, row) {
                    break;
                }
            }
            let (entries, next_cursor) = page.finish(false);
            return Ok(json!({"entries":entries,"next_cursor":next_cursor}));
        }
        Scope::Track(area, id) => {
            target(ctx, area, id).await?;
            let root = format!("/areas/{area}/tracks/{id}/");
            vec![
                row("report.md", "Report", &format!("{root}report.md"), "file"),
                row(
                    "report-edits",
                    "Report edits",
                    &format!("{root}report-edits/"),
                    "dir",
                ),
            ]
        }
        Scope::ChangesDir | Scope::EditsDir(..) => {
            if let Scope::EditsDir(area, id) = scope {
                target(ctx, area, id).await?;
            }
            if args.get("cursor").is_some() {
                return Err(RpcError::invalid_params("date directories have no cursor"));
            }
            return Ok(json!({"entries":[],"next_cursor":null,"date_pattern":"YYYY-MM-DD.json"}));
        }
        _ => return Err(RpcError::invalid_params("path is not a directory")),
    };
    listing(rows, args)
}

pub(super) async fn cat(
    ctx: &Arc<AppContext>,
    scope: Scope<'_>,
    args: &Value,
    zone: chrono_tz::Tz,
) -> Result<Value, RpcError> {
    match scope {
        Scope::Report(area, id) => {
            reject(args, &["step", "cursor", "through_event_id"])?;
            report(ctx, &target(ctx, area, id).await?, args).await
        }
        Scope::Changes(file) | Scope::Edits(_, _, file) => {
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
            let value = if let Scope::Edits(area, id, _) = scope {
                target(ctx, area, id).await?;
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
