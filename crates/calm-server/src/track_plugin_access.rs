//! User-controlled extra plugin authority, independent of a Track's owning plugin.
use crate::{
    ask::StoredAsk,
    error::{CalmError, Result},
    event::{AskAction, AskAnswer, AskDelivery, AskQuestion, Event, EventScope},
    ids::CardId,
    model::Track,
    plugin_host::{Manifest, manifest::AgentToolsScope},
};
use sha2::{Digest, Sha256};
use sqlx::{Row, Sqlite, Transaction};
use std::collections::BTreeSet;

pub(crate) fn digest(manifest: &Manifest) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(calm_types::report_blocks::canonical_json(
            &serde_json::to_value(manifest)?
        ))
    ))
}

pub(crate) async fn grants(
    db: Option<&crate::db::sqlite::SqliteDb>,
    track: &str,
) -> Result<Vec<(String, String, String)>> {
    let db = db.ok_or_else(|| CalmError::Internal("plugin access requires SQLite".into()))?;
    grants_from(db.read_pool(), track).await
}

pub(crate) async fn grants_from<'e, E: sqlx::SqliteExecutor<'e>>(
    executor: E,
    track: &str,
) -> Result<Vec<(String, String, String)>> {
    Ok(sqlx::query_as(
        concat!("SELECT a.plugin_id, a.manifest_sha256, p.manifest FROM track_plugin_access a JOIN plugins p ON ","p.id=a.plugin_id WHERE a.track_id=? AND a.granted=1"),
    )
    .bind(track)
    .fetch_all(executor)
    .await?)
}

pub(crate) async fn current_grants(
    db: Option<&crate::db::sqlite::SqliteDb>,
    track: &str,
    host: &crate::plugin_host::PluginHost,
) -> Result<BTreeSet<String>> {
    filter_grants(grants(db, track).await?, host)
}

pub(crate) fn filter_grants(
    rows: Vec<(String, String, String)>,
    host: &crate::plugin_host::PluginHost,
) -> Result<BTreeSet<String>> {
    let mut result = BTreeSet::new();
    for (id, approved, declared) in rows {
        let installed =
            Manifest::parse(&declared).map_err(|e| CalmError::Internal(e.to_string()))?;
        if let Some(manifest) = host.registry().get(&id)
            && manifest.agent_tools_scope == AgentToolsScope::Enabled
            && digest(&installed)? == approved
        {
            result.insert(id);
        }
    }
    Ok(result)
}

async fn decision_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &str,
    plugin: &str,
) -> Result<(bool, String, i64)> {
    let row = sqlx::query("SELECT granted, manifest_sha256, revision FROM track_plugin_access WHERE track_id=? AND plugin_id=?")
        .bind(track).bind(plugin).fetch_optional(&mut **tx).await?;
    Ok(row
        .map(|row| (row.get::<bool, _>(0), row.get(1), row.get(2)))
        .unwrap_or((false, String::new(), 0)))
}

/// Read the stored enabled manifest inside the same transaction as the ask/answer.
async fn eligible_tx(
    tx: &mut Transaction<'_, Sqlite>,
    plugin: &str,
    require_enabled: bool,
) -> Result<Manifest> {
    let row = sqlx::query("SELECT manifest, enabled FROM plugins WHERE id=?")
        .bind(plugin)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("plugin {plugin}")))?;
    if require_enabled && !row.get::<bool, _>("enabled") {
        return Err(CalmError::BadRequest(
            "enable the plugin in Settings first".into(),
        ));
    }
    let manifest = Manifest::parse(row.get::<&str, _>("manifest"))
        .map_err(|e| CalmError::Internal(e.to_string()))?;
    if manifest.agent_tools_scope != AgentToolsScope::Enabled {
        return Err(CalmError::BadRequest(
            "owner-only plugins cannot receive extra Track authorization".into(),
        ));
    }
    Ok(manifest)
}

pub(crate) fn questions(manifest: &Manifest, enabled: bool) -> Vec<AskQuestion> {
    #[derive(serde::Deserialize)]
    struct Copy {
        allow: AskQuestion,
        revoke: AskQuestion,
    }
    let copy: Copy = serde_json::from_str(include_str!("../prompts/plugin-access.json.md"))
        .expect("compiled plugin access question copy");
    let mut question = if enabled { copy.allow } else { copy.revoke };
    question.title = crate::ask::clip_title(
        &crate::prompts::render_named(
            &question.title,
            &[
                ("name", &manifest.display_name),
                ("plugin_id", &manifest.id),
            ],
        )
        .expect("compiled plugin access title"),
    );
    question.detail = Some(
        crate::prompts::render_named(
            question.detail.as_deref().expect("compiled detail"),
            &[("description", manifest.description.as_deref().unwrap_or(""))],
        )
        .expect("compiled plugin access detail"),
    );
    vec![question]
}

/// Returns an existing pending ask, no ask when already decided, or a new canonical ask.
/// The tool's caller inserts its event under the authenticated Planner in this transaction.
pub(crate) async fn request_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card: &CardId,
    plugin: &str,
    enabled: bool,
) -> Result<(Option<i64>, Option<(EventScope, Event)>)> {
    let track_id: String = sqlx::query_scalar("SELECT track_id FROM cards WHERE id=?")
        .bind(card.as_str())
        .fetch_one(&mut **tx)
        .await?;
    let track = crate::db::sqlite::track_get_tx(tx, &track_id.clone().into()).await?;
    if track.closed_at.is_some()
        || track.plugin_scope.is_none()
        || track.plugin_scope.as_deref() == Some(plugin)
    {
        return Err(CalmError::BadRequest(
            "extra authorization requires an open plugin-owned Track and a different plugin".into(),
        ));
    }
    let manifest = eligible_tx(tx, plugin, enabled).await?;
    let manifest_sha256 = digest(&manifest)?;
    let (granted, approved, revision) = decision_tx(tx, &track_id, plugin).await?;
    if (enabled && granted && approved == manifest_sha256) || (!enabled && !granted) {
        return Ok((None, None));
    }
    let action = AskAction::TrackPluginAccess {
        plugin_id: plugin.into(),
        enabled,
        manifest_sha256,
        expected_revision: revision,
    };
    let pending: Option<i64> = sqlx::query_scalar(concat!(
        "SELECT r.id FROM events r WHERE r.scope_track=? AND r.kind='ask.requested' ",
        "AND json_extract(r.payload,'$.action.kind')='track_plugin_access' ",
        "AND json_extract(r.payload,'$.action.plugin_id')=? ",
        "AND json_extract(r.payload,'$.action.enabled')=? ",
        "AND json_extract(r.payload,'$.action.manifest_sha256')=? ",
        "AND json_extract(r.payload,'$.action.expected_revision')=? AND ",
        crate::ask::ask_open_sql!(),
        " ORDER BY r.id LIMIT 1"
    ))
    .bind(&track_id)
    .bind(plugin)
    .bind(enabled)
    .bind(digest(&manifest)?)
    .bind(revision)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(id) = pending {
        return Ok((Some(id), None));
    }
    let event = crate::ask::requested_tx(
        tx,
        card,
        questions(&manifest, enabled),
        AskDelivery::Wake,
        None,
        Some(action),
    )
    .await?;
    Ok((None, Some(event)))
}

/// Only an option click on this canonical action can change authority. Ordinary asks and text do nothing.
pub(crate) async fn answer_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &Track,
    ask: &StoredAsk,
    answers: &[AskAnswer],
) -> Result<()> {
    let Some(AskAction::TrackPluginAccess {
        plugin_id,
        enabled,
        manifest_sha256,
        expected_revision,
    }) = &ask.action
    else {
        return Ok(());
    };
    if !matches!(answers, [AskAnswer::Option(0)] | [AskAnswer::Option(1)]) {
        return Err(CalmError::BadRequest(
            "plugin authorization requires an explicit option choice".into(),
        ));
    }
    // Refusal always settles the question, even after the plugin was disabled/removed.
    if matches!(answers, [AskAnswer::Option(1)]) {
        return Ok(());
    }
    let manifest = eligible_tx(tx, plugin_id, *enabled).await?;
    if ask.delivery != AskDelivery::Wake
        || ask.questions != questions(&manifest, *enabled)
        || digest(&manifest)? != *manifest_sha256
        || track.closed_at.is_some()
        || track.plugin_scope.is_none()
        || track.plugin_scope.as_deref() == Some(plugin_id)
    {
        return Err(CalmError::BadRequest(
            "plugin authorization changed; decline and request again".into(),
        ));
    }
    let (_, _, revision) = decision_tx(tx, track.id.as_str(), plugin_id).await?;
    if revision != *expected_revision {
        return Err(CalmError::BadRequest(
            "plugin authorization changed; decline and request again".into(),
        ));
    }
    sqlx::query(concat!("INSERT INTO track_plugin_access(track_id,plugin_id,granted,manifest_sha256,revision) ","VALUES(?,?,?,?,?) ON CONFLICT(track_id,plugin_id) DO UPDATE SET ","granted=excluded.granted,manifest_sha256=excluded.manifest_sha256,revision=excluded.revision"))
        .bind(track.id.as_str()).bind(plugin_id).bind(enabled).bind(manifest_sha256).bind(revision+1)
        .execute(&mut **tx).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn track_plugin_access_manifest_digest_is_stable_across_independent_parses() {
        let raw = json!({"manifest_version":2,"id":"digest-app","version":"0.1.0","kind":"app",
            "min_kernel_version":"0.1.0","display_name":"Digest app","entrypoint":{"command":"bin/app"},"exposes_tools":[],
            "views":[{"view_id":"main","title":"Main","scope":"card",
                "csp":{"style-src":["a"],"img-src":["b"],"font-src":["c"],"media-src":["d"]}}]})
        .to_string();
        let first = Manifest::parse(&raw).unwrap();
        let expected = digest(&first).unwrap();
        for _ in 0..40 {
            let parsed = Manifest::parse(&raw).unwrap();
            assert_eq!(digest(&parsed).unwrap(), expected);
        }
    }
}
