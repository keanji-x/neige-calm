//! Manifest version 7 `"approval": "delegated"`: an `app` plugin's tool whose Codex approval the
//! owner's install and enable grants. The tool's annotations stay truthful; the kernel writes
//! the grant into the shared Codex home's kernel MCP entry for every installed and enabled plugin,
//! and stays the live role, scope and running-state authority for every call.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::manifest::Manifest;

/// The first `manifest_version` whose tools may declare `approval`.
pub const MIN_MANIFEST_VERSION: u32 = 7;

/// A manifest tool's `approval`. The only value is `delegated`; any other is a parse error, and
/// an absent key leaves approval to the agent runtime's own policy.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ToolApproval {
    Delegated,
}

/// The minted names (`plugin_<id>_<tool>`) of the delegated tools among `enabled`, the manifests
/// of the installed and enabled plugins. `claimed` holds the names another source serves, such as
/// a compiled built-in's native tools. A name that more than one tool claims is left out: the
/// grant is keyed by name, so it would also cover a tool that did not ask for it.
pub fn delegated_tool_names<'a>(
    enabled: impl IntoIterator<Item = &'a Manifest>,
    claimed: &BTreeSet<String>,
) -> BTreeSet<String> {
    // Minted name -> (claimants, all delegated).
    let mut minted: BTreeMap<String, (usize, bool)> = claimed
        .iter()
        .map(|name| (name.clone(), (1, false)))
        .collect();
    for manifest in enabled {
        for tool in &manifest.exposes_tools {
            let name = crate::results::registry_name(&manifest.id, &tool.name);
            let entry = minted.entry(name).or_insert((0, true));
            entry.0 += 1;
            entry.1 &= tool.approval == Some(ToolApproval::Delegated);
        }
    }
    minted
        .into_iter()
        .filter(|(_, (claimants, delegated))| *claimants == 1 && *delegated)
        .map(|(name, _)| name)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::ManifestError;
    use serde_json::{Value, json};

    fn manifest_with(version: u32, kind: &str, tools: Value) -> Result<Manifest, ManifestError> {
        let mut v = json!({
            "manifest_version": version,
            "id": "probe",
            "version": "0.1.0",
            "min_kernel_version": "0.1.0",
            "display_name": "Probe",
            "kind": kind,
            "exposes_tools": tools,
        });
        match kind {
            "app" => v["entrypoint"] = json!({ "command": "bin/x" }),
            "mcp-http" => v["mcp_http"] = json!({ "url": "https://mcp.example.com/mcp" }),
            "cli-query" => {
                v["cli_query"] = json!({
                    "command": "query",
                    "tools": [{
                        "name": "q",
                        "input_schema": { "type": "object", "properties": {} },
                        "args": ["q"]
                    }],
                })
            }
            _ => {}
        }
        Manifest::parse(&v.to_string())
    }

    fn app(version: u32, tools: Value) -> Result<Manifest, ManifestError> {
        manifest_with(version, "app", tools)
    }

    fn field(err: ManifestError) -> String {
        match err {
            ManifestError::Invalid { field, .. } => field,
            other => panic!("wrong variant: {other:?}"),
        }
    }

    fn none() -> BTreeSet<String> {
        BTreeSet::new()
    }

    fn named(id: &str, tools: &[(&str, bool)]) -> Manifest {
        let tools: Vec<Value> = tools
            .iter()
            .map(|(name, delegated)| {
                let mut tool = json!({ "name": name });
                if *delegated {
                    tool["approval"] = json!("delegated");
                }
                tool
            })
            .collect();
        let mut m = app(7, json!(tools)).unwrap();
        m.id = id.to_string();
        m
    }

    #[test]
    fn approval_is_accepted_from_version_7() {
        let m = app(
            7,
            json!([{ "name": "open", "approval": "delegated" }, { "name": "ls" }]),
        )
        .unwrap();
        assert_eq!(m.exposes_tools[0].approval, Some(ToolApproval::Delegated));
        assert_eq!(m.exposes_tools[1].approval, None);
        let err = app(6, json!([{ "name": "open", "approval": "delegated" }])).unwrap_err();
        assert_eq!(field(err), "manifest_version");
        // Absent stays legal at every version.
        assert!(app(6, json!([{ "name": "open" }])).is_ok());
    }

    #[test]
    fn any_other_approval_value_is_refused() {
        // `null` is an absent key, as for every optional manifest key.
        for bad in [
            json!("approve"),
            json!("Delegated"),
            json!(""),
            json!(true),
            json!({}),
        ] {
            let err = app(7, json!([{ "name": "open", "approval": bad }]));
            assert!(matches!(err, Err(ManifestError::Json(_))), "{bad}: {err:?}");
        }
    }

    #[test]
    fn only_app_tools_may_delegate_approval() {
        let delegated = json!([{ "name": "q", "approval": "delegated" }]);
        for kind in ["mcp-http", "cli-query", "builtin"] {
            let err = manifest_with(7, kind, delegated.clone()).unwrap_err();
            assert_eq!(field(err), "exposes_tools", "{kind}");
            // The same manifest without the key is legal, so the refusal is the key's.
            manifest_with(7, kind, json!([{ "name": "q" }])).unwrap();
        }
    }

    #[test]
    fn the_approval_key_round_trips_and_is_omitted_when_absent() {
        let m = app(
            7,
            json!([{ "name": "open", "approval": "delegated" }, { "name": "ls" }]),
        )
        .unwrap();
        let json = m.to_json();
        assert_eq!(json["exposes_tools"][0]["approval"], "delegated");
        assert!(json["exposes_tools"][1].get("approval").is_none());
    }

    #[test]
    fn only_delegated_tools_are_named_and_names_are_minted() {
        let names = delegated_tool_names(
            &[named("probe", &[("web.fetch", true), ("write", false)])],
            &none(),
        );
        assert_eq!(
            names,
            BTreeSet::from(["plugin_probe_web_fetch".to_string()])
        );
    }

    #[test]
    fn two_plugins_combine() {
        let names = delegated_tool_names(
            &[
                named("alpha", &[("open", true), ("ls", false)]),
                named("beta", &[("input", true)]),
            ],
            &none(),
        );
        assert_eq!(
            names,
            BTreeSet::from([
                "plugin_alpha_open".to_string(),
                "plugin_beta_input".to_string()
            ])
        );
        assert!(delegated_tool_names(&[], &none()).is_empty());
    }

    #[test]
    fn a_name_two_enabled_tools_mint_is_never_granted() {
        // `svc.tools`/`x` and `svc`/`tools_x` both mint `plugin_svc_tools_x`.
        let delegated = named("svc.tools", &[("x", true)]);
        let other = named("svc", &[("tools_x", false)]);
        assert!(delegated_tool_names(&[delegated.clone(), other], &none()).is_empty());
        let both = named("svc", &[("tools_x", true)]);
        assert!(delegated_tool_names(&[delegated.clone(), both], &none()).is_empty());
        assert_eq!(delegated_tool_names(&[delegated], &none()).len(), 1);
    }

    #[test]
    fn a_name_another_source_serves_is_never_granted() {
        // A compiled built-in native such as `plugin_gitforge_publish`.
        let delegated = named("gitforge", &[("publish", true), ("open", true)]);
        let natives = BTreeSet::from(["plugin_gitforge_publish".to_string()]);
        assert_eq!(
            delegated_tool_names(&[delegated], &natives),
            BTreeSet::from(["plugin_gitforge_open".to_string()])
        );
    }

    #[test]
    fn a_forge_action_may_not_delegate_approval() {
        let err = app(
            7,
            json!([{ "name": "ship", "kind": "forge-action", "approval": "delegated" }]),
        )
        .unwrap_err();
        assert_eq!(field(err), "exposes_tools");
        app(7, json!([{ "name": "ship", "kind": "forge-action" }])).unwrap();
    }
}
