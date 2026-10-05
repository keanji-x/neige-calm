//! Plugin standing Planner instructions (#2104 K2). An enabled plugin instructs a Track's Planner
//! when the Track sees its tools and either the built-in rule holds or the Track's current report
//! references the plugin. The text is the stored manifest of the enabled plugin row, so it never
//! waits for the plugin host. Documentation only: nothing here enables or authorizes a tool.

use std::collections::BTreeSet;

use calm_types::report_blocks::live_refs::referenced_plugin_ids;

use crate::db::Repo;
use crate::error::{CalmError, Result};
use crate::mcp_server::tool_visibility::plugin_scope_for_track_row;
use crate::model::Track;
use crate::plugin_host::{Manifest, PluginHost};

/// Every byte the plugin section appends, the omission notice included.
pub(crate) const PLUGIN_INSTRUCTIONS_CAP: usize = 4096;

/// Appended once when any plugin is left out. Its bytes are always reserved, so the section
/// never exceeds [`PLUGIN_INSTRUCTIONS_CAP`].
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

/// Append the plugin section to `instructions`, joined like every other fragment.
pub(crate) async fn append_plugin_instructions(
    repo: &dyn Repo,
    plugin: &PluginHost,
    track: &Track,
    instructions: &mut String,
) -> Result<()> {
    let plugins = instructing_plugins(repo, plugin, track).await?;
    let section = render_section(&plugins);
    if !section.is_empty() {
        instructions.push_str("\n\n");
        instructions.push_str(&section);
    }
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
    let scope = plugin_scope_for_track_row(track, Some(plugin)).await;
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
        .filter(|manifest| {
            plugin_documents_track(manifest, track) || referenced.contains(&manifest.id)
        })
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

/// Blocks are admitted in id order while they fit beside the reserved notice; once one does not,
/// it and every later plugin are omitted, each logged, and the notice is appended once.
fn render_section(plugins: &[(String, String)]) -> String {
    let budget = PLUGIN_INSTRUCTIONS_CAP - OMITTED_NOTICE.len();
    let mut section = String::new();
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
