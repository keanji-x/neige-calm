//! Plugin standing Planner instructions (#2104 K2). An enabled plugin instructs a Track's Planner
//! when the Track sees its tools and either the built-in rule holds or the Track's current report
//! references the plugin. The text is the stored manifest of the enabled plugin row, so it never
//! waits for the plugin host. Documentation only: nothing here enables or authorizes a tool.

use std::collections::BTreeSet;

use calm_types::report_blocks::live_refs::referenced_plugin_ids;

use crate::db::Repo;
use crate::error::{CalmError, Result};
use crate::mcp_server::tool_visibility::{
    TrackPluginScope, plugin_scope_for_track_row_with_grants,
};
use crate::model::Track;
use crate::plugin_host::{Manifest, PluginHost};

/// Every byte the plugin section appends to the Planner instructions: the [`JOINER`], the blocks
/// and the [`OMITTED_NOTICE`].
pub(crate) const PLUGIN_INSTRUCTIONS_CAP: usize = 4096;

/// Separates the section from the preceding fragment, like every other fragment.
const JOINER: &str = "\n\n";

/// Appended once when any plugin is left out. Its bytes and the [`JOINER`]'s are always reserved,
/// so the section never exceeds [`PLUGIN_INSTRUCTIONS_CAP`].
pub(crate) const OMITTED_NOTICE: &str =
    "## Plugin instructions omitted (over budget); see the server log\n";

/// The built-in rule: the Track is scoped to the plugin or was saved from one of its templates.
pub(crate) fn plugin_documents_track(manifest: &Manifest, track: &Track) -> bool {
    track.plugin_scope.as_deref() == Some(manifest.id.as_str())
        || manifest
            .templates
            .iter()
            .any(|template| Some(template.id.as_str()) == track.template_id.as_deref())
}

/// Authorization precedes every documentation reason, including template-id matches.
fn instructs_track(
    scope: &TrackPluginScope,
    manifest: &Manifest,
    track: &Track,
    approved: &BTreeSet<String>,
    referenced: &BTreeSet<String>,
) -> bool {
    scope.allows_manifest(manifest)
        && (plugin_documents_track(manifest, track)
            || approved.contains(&manifest.id)
            || referenced.contains(&manifest.id))
}

/// Append the plugin section, joiner included, to `instructions`.
pub(crate) async fn append_plugin_instructions(
    repo: &dyn Repo,
    plugin: &PluginHost,
    track: &Track,
    instructions: &mut String,
) -> Result<()> {
    let plugins = instructing_plugins(repo, plugin, track).await?;
    instructions.push_str(&render_section(&plugins));
    Ok(())
}

/// `(plugin id, text)` of every plugin that instructs this Track's Planner, in id order.
async fn instructing_plugins(
    repo: &dyn Repo,
    plugin: &PluginHost,
    track: &Track,
) -> Result<Vec<(String, String)>> {
    let mut stored = Vec::new();
    for row in repo.plugins_list_all().await? {
        if !row.enabled {
            continue;
        }
        match Manifest::parse(&row.manifest.to_string()) {
            Ok(manifest) if manifest.planner_instructions.is_some() => stored.push(manifest),
            Ok(_) => {}
            Err(error) => tracing::warn!(
                target: "planner_harness::plugin_instructions",
                plugin_id = %row.id,
                %error,
                "stored plugin manifest does not parse; it instructs no Planner"
            ),
        }
    }
    if stored.is_empty() {
        return Ok(Vec::new());
    }
    let scope =
        plugin_scope_for_track_row_with_grants(repo.sqlite_db().as_ref(), track, Some(plugin))
            .await;
    // Grants include earlier template owners as well as individually approved plugins.
    let extra_plugins =
        crate::track_plugin_access::current_grants(repo.sqlite_db().as_ref(), track, plugin)
            .await?;
    stored.retain(|manifest| scope.allows_manifest(manifest));
    stored.sort_by(|a, b| a.id.cmp(&b.id));
    let referenced = if stored
        .iter()
        .all(|manifest| plugin_documents_track(manifest, track))
    {
        BTreeSet::new()
    } else {
        report_references(repo, track).await
    };
    Ok(stored
        .into_iter()
        .filter(|manifest| instructs_track(&scope, manifest, track, &extra_plugins, &referenced))
        .filter_map(|manifest| Some((manifest.id, manifest.planner_instructions?)))
        .collect())
}

/// The plugins the Track's current report references. A report that cannot be read references
/// nothing: the instructions are documentation and must not block a Planner start.
async fn report_references(repo: &dyn Repo, track: &Track) -> BTreeSet<String> {
    let blocks = async {
        let (_, card, _) =
            crate::track_report::resolve_report_for_track(repo, track.id.as_str()).await?;
        let doc =
            crate::track_report_read::load_report_doc_snapshot(repo, card.id.as_str()).await?;
        Ok::<_, CalmError>(doc.blocks)
    }
    .await;
    match blocks {
        Ok(blocks) => referenced_plugin_ids(&blocks),
        Err(error) => {
            tracing::warn!(
                target: "planner_harness::plugin_instructions",
                track_id = %track.id,
                %error,
                "track report unreadable; no plugin is instructed by report reference"
            );
            BTreeSet::new()
        }
    }
}

/// The whole appended section, empty when no plugin instructs. After the joiner, blocks are
/// admitted in id order while they fit beside the reserved notice; once one does not, it and every
/// later plugin are omitted, each logged, and the notice is appended once.
fn render_section(plugins: &[(String, String)]) -> String {
    if plugins.is_empty() {
        return String::new();
    }
    let budget = PLUGIN_INSTRUCTIONS_CAP - OMITTED_NOTICE.len();
    let mut section = String::from(JOINER);
    let mut omitted = false;
    for (id, text) in plugins {
        let block = format!("## Plugin {id}\n{text}\n");
        if !omitted && section.len() + block.len() <= budget {
            section.push_str(&block);
            continue;
        }
        omitted = true;
        tracing::warn!(
            target: "planner_harness::plugin_instructions",
            plugin_id = %id,
            bytes = block.len(),
            cap = PLUGIN_INSTRUCTIONS_CAP,
            "plugin Planner instructions omitted: over the aggregate budget"
        );
    }
    if omitted {
        section.push_str(OMITTED_NOTICE);
    }
    section
}

/// Configuration wakes use the same standing documentation owners as a harness
/// start, with grants read after rebinding in the caller's writer transaction.
pub(crate) async fn append_configuration_instructions_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    track: &Track,
    host: &PluginHost,
    instructions: &mut String,
) -> Result<()> {
    let scope =
        crate::mcp_server::tool_visibility::plugin_scope_for_track_row_tx(tx, track, host).await?;
    let referenced = match crate::track_report::report_blocks_snapshot_tx(tx, track.id.as_str())
        .await
    {
        Ok((_, blocks)) => referenced_plugin_ids(&blocks),
        Err(error) => {
            tracing::warn!(track_id = %track.id, %error, "track report unreadable; no plugin is instructed by report reference");
            BTreeSet::new()
        }
    };
    let rows = crate::track_plugin_access::grants_from(&mut **tx, track.id.as_str()).await?;
    let approved = crate::track_plugin_access::filter_grants(rows, host).await?;
    let active = host.running_plugin_ids().await;
    for component in crate::builtin_plugins::catalog() {
        if active.contains(&component.manifest().id)
            && instructs_track(&scope, component.manifest(), track, &approved, &referenced)
        {
            instructions.push_str(JOINER);
            instructions.push_str(component.instructions());
        }
    }
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT id,manifest FROM plugins WHERE enabled=1")
            .fetch_all(&mut **tx)
            .await?;
    let mut plugins = Vec::new();
    for (id, raw) in rows {
        if !active.contains(&id) {
            continue;
        }
        let manifest =
            Manifest::parse(&raw).map_err(|error| CalmError::Internal(error.to_string()))?;
        if instructs_track(&scope, &manifest, track, &approved, &referenced)
            && let Some(text) = manifest.planner_instructions
        {
            plugins.push((id, text));
        }
    }
    plugins.sort_by(|a, b| a.0.cmp(&b.0));
    instructions.push_str(&render_section(&plugins));
    Ok(())
}
