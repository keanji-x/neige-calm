//! Bound-identity navigation of the Report namespace. Content stays with its owners.
pub mod name;
use crate::mcp_server::{
    framing::RpcError,
    registry::{AppContext, ToolCallIdentity},
    tools::track_file,
};
use crate::model::{AreaKind, Track};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    Root,
    System,
    Area(String),
    Track(String, String),
    Bound(String),
    Report(String, String),
    ChangesDir,
    Changes(String),
    EditsDir(String, String),
    Edits(String, String, String),
}
fn bad() -> RpcError {
    RpcError::invalid_params("path not available in this view")
}
fn internal(e: impl std::fmt::Display) -> RpcError {
    RpcError::internal(e.to_string())
}

/// Resolve a segment against the complete sibling set, before filtering or paging.
pub fn lookup(segment: &str, siblings: &[(&str, &str)]) -> Result<String, RpcError> {
    lookup_visible(segment, siblings, |_| true)
}

fn lookup_visible(
    segment: &str,
    siblings: &[(&str, &str)],
    visible: impl Fn(&str) -> bool,
) -> Result<String, RpcError> {
    let parsed = name::parse(segment).map_err(RpcError::invalid_params)?;
    let names = name::segments(siblings);
    if let Some(index) = names.iter().position(|name| name == segment) {
        return if visible(siblings[index].1) {
            Ok(siblings[index].1.to_string())
        } else {
            Err(bad())
        };
    }
    let candidates: Vec<_> = siblings
        .iter()
        .zip(&names)
        .filter(|((title, id), _)| *title == parsed.title && visible(id))
        .map(|(_, name)| name.as_str())
        .collect();
    if parsed.suffix.is_none()
        && !candidates.is_empty()
        && siblings
            .iter()
            .filter(|(title, _)| *title == parsed.title)
            .count()
            > 1
    {
        return Err(RpcError::invalid_params(format!(
            "ambiguous name; use one of: {}",
            candidates.join(", ")
        )));
    }
    Err(bad())
}

pub async fn areas(ctx: &AppContext) -> Result<Vec<(crate::model::Area, String)>, RpcError> {
    let rows = ctx.repo.areas_list_user_visible().await.map_err(internal)?;
    let names = name::segments(
        &rows
            .iter()
            .map(|a| (a.name.as_str(), a.id.as_str()))
            .collect::<Vec<_>>(),
    );
    Ok(rows.into_iter().zip(names).collect())
}
pub async fn tracks(ctx: &AppContext, area: &str) -> Result<Vec<(Track, String)>, RpcError> {
    let rows = ctx.repo.tracks_by_area(area).await.map_err(internal)?;
    let names = name::segments(
        &rows
            .iter()
            .map(|t| (t.title.as_str(), t.id.as_str()))
            .collect::<Vec<_>>(),
    );
    Ok(rows.into_iter().zip(names).collect())
}
pub async fn user_area(ctx: &AppContext, area: &str) -> Result<(), RpcError> {
    let a = ctx
        .repo
        .area_get(area)
        .await
        .map_err(internal)?
        .ok_or_else(bad)?;
    if a.kind != AreaKind::User {
        return Err(RpcError::forbidden("Report is outside user Areas"));
    }
    Ok(())
}
pub async fn area_path(ctx: &AppContext, area: &str) -> Result<String, RpcError> {
    let (_, name) = areas(ctx)
        .await?
        .into_iter()
        .find(|(a, _)| a.id.as_str() == area)
        .ok_or_else(bad)?;
    Ok(format!("/{name}/"))
}
pub async fn track_path(ctx: &AppContext, area: &str, track: &str) -> Result<String, RpcError> {
    let root = area_path(ctx, area).await?;
    let (_, name) = tracks(ctx, area)
        .await?
        .into_iter()
        .find(|(t, _)| t.id.as_str() == track)
        .ok_or_else(bad)?;
    Ok(format!("{root}{name}/"))
}

/// Start from the actual live-bound IDs. Never normalize away a node before checking it.
pub async fn resolve(
    ctx: &Arc<AppContext>,
    identity: &ToolCallIdentity,
    bound: &Track,
    path: &str,
) -> Result<Scope, RpcError> {
    if path.is_empty() {
        return Err(bad());
    }
    let mut stack = if path.starts_with('/') {
        vec![Scope::Root]
    } else {
        vec![
            Scope::Root,
            Scope::Area(bound.area_id.to_string()),
            Scope::Track(bound.area_id.to_string(), bound.id.to_string()),
        ]
    };
    let raw = path.strip_prefix('/').unwrap_or(path);
    let raw = raw.strip_suffix('/').unwrap_or(raw);
    if raw.is_empty() && path == "/" {
        return Ok(Scope::Root);
    }
    let parts: Vec<_> = raw.split('/').collect();
    for (i, part) in parts.iter().enumerate() {
        let current = stack.last().unwrap();
        directory(ctx, identity, bound, current).await?;
        match *part {
            "" => return Err(bad()),
            "." => continue,
            ".." => {
                if stack.len() == 1 {
                    return Err(bad());
                }
                stack.pop();
                continue;
            }
            _ => {}
        }
        let next = match current {
            Scope::Root if *part == ".neige" => Scope::System,
            Scope::Root => {
                let rows = areas(ctx).await?;
                Scope::Area(lookup(
                    part,
                    &rows
                        .iter()
                        .map(|(a, _)| (a.name.as_str(), a.id.as_str()))
                        .collect::<Vec<_>>(),
                )?)
            }
            Scope::System if *part == "report-changes" => Scope::ChangesDir,
            Scope::Area(area) => {
                user_area(ctx, area).await?;
                let rows = tracks(ctx, area).await?;
                let id = lookup_visible(
                    part,
                    &rows
                        .iter()
                        .map(|(t, _)| (t.title.as_str(), t.id.as_str()))
                        .collect::<Vec<_>>(),
                    |id| {
                        rows.iter().any(|(t, _)| {
                            t.id.as_str() == id && crate::track_access::user_visible_track(t)
                        })
                    },
                )?;
                Scope::Track(area.clone(), id)
            }
            Scope::Track(area, id) => match *part {
                "report.md" => Scope::Report(area.clone(), id.clone()),
                "report-edits" => Scope::EditsDir(area.clone(), id.clone()),
                _ if id == bound.id.as_str() => Scope::Bound(part.to_string()),
                _ => return Err(bad()),
            },
            Scope::Bound(parent) => Scope::Bound(format!("{parent}/{part}")),
            Scope::ChangesDir => Scope::Changes(part.to_string()),
            Scope::EditsDir(area, id) => Scope::Edits(area.clone(), id.clone(), part.to_string()),
            _ => return Err(bad()),
        };
        stack.push(next);
        // Final own leaf still goes through the content owner; intermediate nodes never do.
        if i + 1 == parts.len() && path.ends_with('/') {
            directory(ctx, identity, bound, stack.last().unwrap()).await?;
        }
    }
    let scope = stack.pop().unwrap();
    if let Scope::Bound(path) = &scope {
        canonical_own_path(path)?;
    }
    if let Scope::Area(area) = &scope {
        user_area(ctx, area).await?;
    }
    match scope {
        Scope::Track(_, ref id) if id == bound.id.as_str() => Ok(Scope::Bound(String::new())),
        other => Ok(other),
    }
}
// Reuse the own protocol's normalizer as a canonicality check, never as a fallback.
fn canonical_own_path(path: &str) -> Result<(), RpcError> {
    if crate::track_fs_view::normalize_path(path) != path {
        return Err(bad());
    }
    Ok(())
}

async fn directory(
    ctx: &Arc<AppContext>,
    identity: &ToolCallIdentity,
    bound: &Track,
    scope: &Scope,
) -> Result<(), RpcError> {
    match scope {
        Scope::Bound(path) => {
            canonical_own_path(path)?;
            if !track_file::own_node_kind(ctx, identity, bound, path)
                .await?
                .is_directory()
            {
                return Err(bad());
            }
        }
        Scope::Report(..) | Scope::Changes(..) | Scope::Edits(..) => return Err(bad()),
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests;
