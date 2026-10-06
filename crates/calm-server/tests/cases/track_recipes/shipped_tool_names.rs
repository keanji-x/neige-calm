//! Every shipped recipe and built-in template names a plugin tool by the minted name the model sees
//! (`plugin_results::registry_name`, #2087 §6), never by the raw upstream name a manifest declares.
//! A raw name stays only as a `neige://plugin/<id>/<raw>` URI segment, which is a protocol id.

use std::path::{Path, PathBuf};

use calm_server::mcp_server::build_default_registry;
use calm_server::plugin_results::registry_name;
use serde_json::Value;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// `(raw, minted)` for every tool of every repository manifest.
fn manifest_tools() -> Vec<(String, String)> {
    let mut tools = Vec::new();
    for entry in std::fs::read_dir(repo().join("plugins")).unwrap() {
        let manifest = entry.unwrap().path().join("manifest.json");
        if !manifest.is_file() {
            continue;
        }
        let manifest: Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
        let id = manifest["id"].as_str().expect("manifest id");
        for tool in manifest["exposes_tools"].as_array().expect("exposes_tools") {
            let raw = tool["name"].as_str().expect("tool name");
            tools.push((raw.to_string(), registry_name(id, raw)));
        }
    }
    assert!(tools.len() > 20, "{tools:?}");
    tools
}

/// The shipped recipe bodies (`plugins/*/*recipe*.md`) and the built-in templates.
fn shipped_bodies() -> Vec<(PathBuf, String)> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(repo().join("plugins")).unwrap() {
        let dir = entry.unwrap().path();
        if !dir.is_dir() {
            continue;
        }
        for file in std::fs::read_dir(&dir).unwrap() {
            let file = file.unwrap().path();
            let name = file.file_name().unwrap().to_string_lossy().into_owned();
            if name.ends_with("recipe.md") {
                files.push(file);
            }
        }
    }
    let templates = repo().join("crates/calm-server/templates/builtin");
    for file in std::fs::read_dir(templates).unwrap() {
        let file = file.unwrap().path();
        if file.extension().is_some_and(|ext| ext == "md") {
            files.push(file);
        }
    }
    files.sort();
    assert!(files.len() >= 5, "{files:?}");
    files
        .into_iter()
        .map(|file| {
            let body = std::fs::read_to_string(&file).unwrap();
            (file, body)
        })
        .collect()
}

#[test]
fn shipped_recipes_name_plugin_tools_by_their_minted_names() {
    let tools = manifest_tools();
    let raw_word = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/');
    let mut raw_hits = Vec::new();
    let mut unknown = Vec::new();
    let minted = regex::Regex::new(r"\bplugin_[A-Za-z0-9_]+").unwrap();
    // A built-in's compiled tools (`plugin_calendar_ls`, `plugin_gitforge_publish`, #2227) are in
    // the kernel registry under their minted names, not in a manifest.
    let compiled: Vec<String> = build_default_registry()
        .descriptors()
        .into_iter()
        .map(|descriptor| descriptor.name)
        .filter(|name| name.starts_with("plugin_"))
        .collect();
    assert!(compiled.len() >= 5, "{compiled:?}");
    for (file, body) in shipped_bodies() {
        for (raw, minted) in &tools {
            for (at, _) in body.match_indices(raw.as_str()) {
                let before = body[..at].chars().next_back();
                let after = body[at + raw.len()..]
                    .trim_start_matches('.')
                    .chars()
                    .next();
                if before.is_some_and(raw_word) || after.is_some_and(raw_word) {
                    continue;
                }
                raw_hits.push(format!("{}: `{raw}` (say `{minted}`)", file.display()));
            }
        }
        for name in minted.find_iter(&body) {
            if !tools.iter().any(|(_, minted)| minted == name.as_str())
                && !compiled.iter().any(|compiled| compiled == name.as_str())
            {
                unknown.push(format!("{}: `{}`", file.display(), name.as_str()));
            }
        }
    }
    assert!(
        raw_hits.is_empty(),
        "raw tool names in prose: {raw_hits:#?}"
    );
    assert!(unknown.is_empty(), "names no manifest mints: {unknown:#?}");
}
