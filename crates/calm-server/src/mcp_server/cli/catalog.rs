//! `neige tool ls|describe` (#2003 §4.5): a literal, bounded lookup over this session's
//! `tools/list` set plus every tool a `neige` command calls, each with the plugin that serves it
//! (#2227). Listing is not a grant.
use super::commands::{JSON, cli_spelling, command_for_tool};
use super::{CliExit, Output};
use crate::mcp_server::{
    registry::{AppContext, ConnectionIdentity, ToolDescriptor, ToolRegistry},
    transport::{PluginOwner, tool_descriptors_for_connection, tool_owner},
};
use crate::plugin_host::PluginRegistry;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::Arc;

/// The CLI-only meta object; it is no tool.
pub(super) const COMMAND_NAME: &str = "tool";
pub(super) const ACTIONS: [&str; 2] = ["ls", "describe"];
pub(super) const FOOTER: &str = "listing is not a grant; the tool's role gate decides";

const PAGE_SIZE: usize = 20;
const LIST_MAX_BYTES: usize = 4096;
const DETAIL_MAX_BYTES: usize = 8192;
const INPUT_MAX_BYTES: usize = 256;

#[derive(Debug)]
enum Query {
    List {
        prefix: String,
        cursor: Option<String>,
    },
    Describe {
        name: String,
    },
}

/// One catalog entry: the descriptor, whether this session's `tools/list` shows it, and the plugin
/// that serves it (`None` for a kernel tool).
#[derive(Clone)]
struct Entry {
    tool: ToolDescriptor,
    listed: bool,
    plugin: Option<PluginOwner>,
}

fn valid_prefix(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with("--")
        && value.len() <= INPUT_MAX_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

fn tool_name(value: &str) -> bool {
    !value.is_empty() && !value.chars().any(char::is_whitespace)
}

const LS_USAGE: &str = "neige tool ls (--prefix PREFIX | --all) [--cursor CURSOR]";
const DESCRIBE_USAGE: &str = "neige tool describe --name NAME";

fn parse(argv: &[String]) -> Result<(Query, bool), String> {
    let json = argv.iter().any(|arg| arg == JSON);
    let args: Vec<&str> = argv
        .iter()
        .map(String::as_str)
        .filter(|arg| *arg != JSON)
        .collect();
    let choices = || format!("use `{LS_USAGE}` or `{DESCRIBE_USAGE}`");
    match args.get(1).copied() {
        Some("describe") if args.len() == 4 && args[2] == "--name" && tool_name(args[3]) => Ok((
            Query::Describe {
                name: args[3].into(),
            },
            json,
        )),
        Some("describe") => Err(format!("use `{DESCRIBE_USAGE}`")),
        Some("ls") => {
            let mut prefix = None;
            let mut cursor = None;
            let mut index = 2;
            while index < args.len() {
                match args[index] {
                    "--all" if prefix.is_none() => {
                        prefix = Some(String::new());
                        index += 1;
                    }
                    "--prefix"
                        if prefix.is_none()
                            && args.get(index + 1).is_some_and(|value| valid_prefix(value)) =>
                    {
                        prefix = Some(args[index + 1].to_string());
                        index += 2;
                    }
                    "--cursor"
                        if cursor.is_none()
                            && args.get(index + 1).is_some_and(|value| tool_name(value)) =>
                    {
                        cursor = Some(args[index + 1].to_string());
                        index += 2;
                    }
                    _ => {
                        return Err(format!(
                            "use `{LS_USAGE}`: a literal --prefix or explicit --all, with optional --cursor"
                        ));
                    }
                }
            }
            prefix
                .map(|prefix| (Query::List { prefix, cursor }, json))
                .ok_or_else(|| format!("tool ls requires --prefix or --all; use `{LS_USAGE}`"))
        }
        Some(action) => Err(format!(
            "unknown action `{action}` for `neige tool`; {}",
            choices()
        )),
        None => Err(format!("`neige tool` needs an action; {}", choices())),
    }
}

/// The session's `tools/list` set plus every CLI-covered tool, by name, each with its owner.
fn entries(
    listed: Vec<ToolDescriptor>,
    registry: &ToolRegistry,
    plugins: Option<&PluginRegistry>,
    running_ids: &BTreeSet<String>,
) -> Result<Vec<Entry>, String> {
    let mut tools: Vec<(ToolDescriptor, bool)> =
        listed.into_iter().map(|tool| (tool, true)).collect();
    for tool in registry.descriptors() {
        if command_for_tool(&tool.name).is_some()
            && !tools.iter().any(|(listed, _)| listed.name == tool.name)
        {
            tools.push((tool, false));
        }
    }
    let mut entries = tools
        .into_iter()
        .map(|(tool, listed)| {
            let plugin =
                tool_owner(registry, plugins, running_ids, &tool.name).map_err(|e| e.message)?;
            Ok(Entry {
                tool,
                listed,
                plugin,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    entries.sort_by(|a, b| a.tool.name.cmp(&b.tool.name));
    Ok(entries)
}

/// `plugin` is the serving plugin's id, `kind` the kind its manifest declares; both null for a
/// kernel tool.
fn ownership(entry: &Entry) -> (Value, Value) {
    match &entry.plugin {
        Some(owner) => (json!(owner.id), json!(owner.kind)),
        None => (Value::Null, Value::Null),
    }
}

fn row(entry: &Entry) -> Value {
    let (plugin, kind) = ownership(entry);
    json!({
        "name": entry.tool.name,
        "cli": cli_spelling(&entry.tool.name),
        "listed": entry.listed,
        "plugin": plugin,
        "kind": kind,
    })
}

fn select(query: Query, entries: Vec<Entry>) -> Result<Value, String> {
    match query {
        Query::Describe { name } => {
            let entry = entries
                .into_iter()
                .find(|entry| entry.tool.name == name)
                .ok_or_else(|| {
                    format!("no tool `{name}` in this session; `neige tool ls --all` lists them")
                })?;
            let mut value = entry.tool.clone().into_mcp_value();
            let object = value.as_object_mut().expect("a descriptor is an object");
            object.insert("cli".into(), json!(cli_spelling(&entry.tool.name)));
            object.insert("listed".into(), json!(entry.listed));
            let (plugin, kind) = ownership(&entry);
            object.insert("plugin".into(), plugin);
            object.insert("kind".into(), kind);
            if value.to_string().len() > DETAIL_MAX_BYTES {
                return Err(
                    "tool declaration exceeds the lookup byte limit; use the client's exact-name tool loading"
                        .into(),
                );
            }
            Ok(value)
        }
        Query::List { prefix, cursor } => {
            if cursor
                .as_ref()
                .is_some_and(|name| !name.starts_with(&prefix))
            {
                return Err("--cursor must be a next_cursor of the requested prefix".into());
            }
            let rows: Vec<(String, Value)> = entries
                .iter()
                .filter(|entry| {
                    entry.tool.name.starts_with(&prefix)
                        && cursor.as_ref().is_none_or(|last| entry.tool.name > *last)
                })
                .map(|entry| (entry.tool.name.clone(), row(entry)))
                .collect();
            let mut count = rows.len().min(PAGE_SIZE);
            loop {
                let next = (count < rows.len())
                    .then(|| count.checked_sub(1).map(|index| rows[index].0.clone()))
                    .flatten();
                let page: Vec<&Value> = rows[..count].iter().map(|(_, row)| row).collect();
                let value = json!({"tools": page, "next_cursor": next});
                if value.to_string().len() <= LIST_MAX_BYTES {
                    return Ok(value);
                }
                if count <= 1 {
                    return Err("one tool row exceeds the lookup byte limit".into());
                }
                count -= 1;
            }
        }
    }
}

/// Text output: one `name  cli-or-—  listed|hidden  kernel|plugin:<id>  kind-or-—` row per tool,
/// or the describe JSON, then the footer.
fn text(value: &Value) -> String {
    let mut out = match value["tools"].as_array() {
        Some(rows) => {
            let mut out: String = rows
                .iter()
                .map(|row| {
                    format!(
                        "{}  {}  {}  {}  {}\n",
                        row["name"].as_str().expect("rows carry a name"),
                        row["cli"].as_str().unwrap_or("—"),
                        if row["listed"] == json!(true) {
                            "listed"
                        } else {
                            "hidden"
                        },
                        row["plugin"]
                            .as_str()
                            .map_or("kernel".to_string(), |id| format!("plugin:{id}")),
                        row["kind"].as_str().unwrap_or("—"),
                    )
                })
                .collect();
            out.push_str(&format!("next_cursor: {}\n", value["next_cursor"]));
            out
        }
        None => format!("{value}\n"),
    };
    out.push_str(FOOTER);
    out.push('\n');
    out
}

pub(super) async fn run(
    ctx: &Arc<AppContext>,
    registry: &ToolRegistry,
    identity: &ConnectionIdentity,
    argv: &[String],
) -> Output {
    let json_mode = argv.iter().any(|arg| arg == JSON);
    let (query, json_mode) = match parse(argv) {
        Ok(query) => query,
        Err(message) => return Output::usage(message, json_mode, Some(COMMAND_NAME)),
    };
    if !matches!(identity, ConnectionIdentity::CardBound(_)) {
        return Output::error(
            CliExit::Failed,
            json_mode,
            "tool lookup requires an active card-bound session".into(),
            json!({"kind":"identity"}),
        );
    }
    let listed = match tool_descriptors_for_connection(ctx, registry, identity, None).await {
        Ok(tools) => tools,
        Err(error) => {
            return Output::error(
                CliExit::Failed,
                json_mode,
                error.message,
                json!({"kind":"identity"}),
            );
        }
    };
    let host = ctx.plugin_host.get();
    let running_ids = match host {
        Some(host) => host.running_plugin_ids().await,
        None => BTreeSet::new(),
    };
    let plugins = host.map(|host| host.registry().as_ref());
    match entries(listed, registry, plugins, &running_ids)
        .and_then(|entries| select(query, entries))
    {
        Err(message) => Output::error(
            CliExit::Failed,
            json_mode,
            message,
            json!({"kind":"catalog"}),
        ),
        Ok(value) if json_mode => Output::success(format!("{value}\n")),
        Ok(value) => Output::success(text(&value)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_server::build_default_registry;
    use crate::model::CardRole;
    use crate::plugin_host::Manifest;
    use crate::plugin_host::manifest::ToolKind;

    fn tool(name: String) -> ToolDescriptor {
        ToolDescriptor {
            name,
            description: "Read only".into(),
            input_schema: json!({"type":"object"}),
            annotations: None,
            roles: &[CardRole::Planner],
            listed_for: &[CardRole::Planner],
        }
    }

    fn listed(names: &[String]) -> Vec<Entry> {
        names
            .iter()
            .cloned()
            .map(|name| Entry {
                tool: tool(name),
                listed: true,
                plugin: Some(PluginOwner {
                    id: "gitforge".into(),
                    kind: Some(ToolKind::ForgeAction),
                }),
            })
            .collect()
    }

    fn page_names(page: &Value) -> Vec<String> {
        page["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["name"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn list_pages_cover_long_names_without_silent_truncation() {
        let names: Vec<String> = (0..43)
            .map(|index| format!("plugin_{}.tool{index:02}", "x".repeat(180)))
            .collect();
        let entries = listed(&names);
        let mut cursor = None;
        let mut seen = Vec::new();
        loop {
            let page = select(
                Query::List {
                    prefix: "plugin_".into(),
                    cursor: cursor.clone(),
                },
                entries.clone(),
            )
            .unwrap();
            assert!(page.to_string().len() <= LIST_MAX_BYTES);
            let batch = page_names(&page);
            assert!(!batch.is_empty() && batch.len() <= PAGE_SIZE);
            seen.extend(batch);
            match page["next_cursor"].as_str() {
                Some(next) => {
                    assert_ne!(cursor.as_deref(), Some(next));
                    cursor = Some(next.into());
                }
                None => break,
            }
        }
        assert_eq!(seen, names);
    }

    #[test]
    fn catalog_names_and_cursors_round_trip_through_the_cli_parser() {
        let names: Vec<String> = (0..43)
            .map(|index| format!("plugin_demo_读取/{index:02}:{}", "x".repeat(300)))
            .collect();
        let entries = listed(&names);
        let mut argv = vec![
            "tool".into(),
            "ls".into(),
            "--prefix".into(),
            "plugin_".into(),
        ];
        let mut seen = Vec::new();
        loop {
            let (query, _) = parse(&argv).unwrap();
            let page = select(query, entries.clone()).unwrap();
            for name in page_names(&page) {
                let (query, _) = parse(&[
                    "tool".into(),
                    "describe".into(),
                    "--name".into(),
                    name.clone(),
                ])
                .unwrap();
                assert_eq!(select(query, entries.clone()).unwrap()["name"], name);
                seen.push(name);
            }
            let Some(cursor) = page["next_cursor"].as_str() else {
                break;
            };
            argv.truncate(4);
            argv.extend(["--cursor".into(), cursor.into()]);
        }
        assert_eq!(seen, names);
    }

    #[test]
    fn details_refuse_oversize_and_keep_optional_annotations_absent() {
        let mut large = tool("neige_large_tool".into());
        large.description = "x".repeat(DETAIL_MAX_BYTES);
        assert!(
            select(
                Query::Describe {
                    name: large.name.clone()
                },
                vec![Entry {
                    tool: large,
                    listed: true,
                    plugin: None,
                }]
            )
            .unwrap_err()
            .contains("byte limit")
        );
        let details = select(
            Query::Describe {
                name: "neige_small_tool".into(),
            },
            listed(&["neige_small_tool".into()]),
        )
        .unwrap();
        assert!(details.get("annotations").is_none());
        assert_eq!(details["inputSchema"], json!({"type":"object"}));
        assert_eq!(
            (&details["cli"], &details["listed"]),
            (&Value::Null, &json!(true))
        );
        assert_eq!(
            (&details["plugin"], &details["kind"]),
            (&json!("gitforge"), &json!("forge-action"))
        );
    }

    /// #2003 §4.5: a hidden CLI-covered tool is still discoverable, marked `listed: false` with its
    /// derived command; a listed tool without a command has `cli: null`.
    #[test]
    fn tool_list_includes_cli_covered_hidden_tools() {
        let registry = build_default_registry();
        let planner = registry.descriptors_listed_for(CardRole::Planner);
        assert!(
            !planner.iter().any(|tool| tool.name == "neige_track_cat"),
            "precondition: track.cat is hidden from the Planner's tools/list"
        );
        let page = select(
            Query::List {
                prefix: "neige_track_".into(),
                cursor: None,
            },
            entries(planner, &registry, None, &BTreeSet::new()).unwrap(),
        )
        .unwrap();
        let rows = page["tools"].as_array().unwrap();
        assert!(
            rows.contains(
                &json!({"name":"neige_track_cat","cli":"neige track cat","listed":false,"plugin":null,"kind":null})
            ),
            "{page}"
        );
        assert!(
            rows.contains(
                &json!({"name":"neige_track_close","cli":"neige track close","listed":true,"plugin":null,"kind":null})
            ),
            "{page}"
        );
        assert!(
            rows.contains(&json!({"name":"neige_track_rename","cli":null,"listed":true,"plugin":null,"kind":null})),
            "{page}"
        );
        let described = select(
            Query::Describe {
                name: "neige_track_cat".into(),
            },
            entries(
                registry.descriptors_listed_for(CardRole::Planner),
                &registry,
                None,
                &BTreeSet::new(),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            (&described["cli"], &described["listed"]),
            (&json!("neige track cat"), &json!(false))
        );
        assert!(text(&page).ends_with(&format!("{FOOTER}\n")));
    }

    /// #2227: every row names the plugin that serves it. A built-in native
    /// (`plugin_gitforge_publish`) answers to its compiled owner with no kind; a manifest tool
    /// carries its declared kind; a name nobody serves is refused rather than shown as a kernel tool.
    #[test]
    fn rows_name_the_serving_plugin_and_its_declared_kind() {
        let registry = build_default_registry();
        let invest = Manifest::parse(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../plugins/invest/manifest.json"
        )))
        .unwrap();
        let plugins = PluginRegistry::builder()
            .with(invest, None)
            .build()
            .with_builtins();
        let descriptor = |name: &str| tool(name.into());
        let listed = [
            "plugin_gitforge_publish",
            "plugin_calendar_add",
            "plugin_gitforge_git_commit",
            "plugin_invest_instrument_add",
            "neige_track_rename",
        ]
        .map(descriptor)
        .to_vec();
        let running: BTreeSet<String> = ["gitforge", "calendar", "invest"].map(String::from).into();
        let rows: Vec<Value> = entries(listed, &registry, Some(&plugins), &running)
            .unwrap()
            .iter()
            .map(row)
            .collect();
        let owner = |name: &str| {
            let row = rows.iter().find(|row| row["name"] == name).unwrap();
            (row["plugin"].clone(), row["kind"].clone())
        };
        assert_eq!(
            owner("plugin_gitforge_publish"),
            (json!("gitforge"), Value::Null)
        );
        assert_eq!(
            owner("plugin_calendar_add"),
            (json!("calendar"), Value::Null)
        );
        assert_eq!(
            owner("plugin_gitforge_git_commit"),
            (json!("gitforge"), json!("forge-action"))
        );
        assert_eq!(
            owner("plugin_invest_instrument_add"),
            (json!("invest"), Value::Null)
        );
        assert_eq!(owner("neige_track_rename"), (Value::Null, Value::Null));
        assert_eq!(owner("neige_track_cat"), (Value::Null, Value::Null));
        let page = json!({"tools": rows, "next_cursor": null});
        assert!(
            text(&page).contains("plugin_gitforge_publish  —  listed  plugin:gitforge  —\n"),
            "{}",
            text(&page)
        );
        assert!(
            text(&page)
                .contains("plugin_gitforge_git_commit  —  listed  plugin:gitforge  forge-action\n")
        );
        assert!(text(&page).contains("neige_track_cat  neige track cat  hidden  kernel  —\n"));
        let ghost = entries(
            vec![descriptor("plugin_ghost_tool")],
            &registry,
            Some(&plugins),
            &running,
        )
        .err()
        .unwrap();
        assert!(ghost.contains("plugin_ghost_tool"), "{ghost}");
    }

    /// #2227 review: the host fences only running plugins against minting one name, so an installed,
    /// stopped plugin may mint a running plugin's name. The row names the running owner, as
    /// dispatch routes it, instead of failing the whole listing as ambiguous.
    #[test]
    fn a_stopped_plugin_minting_the_same_name_leaves_the_running_owner() {
        let manifest = |id: &str, tool: &str| {
            Manifest::parse(&format!(
                r#"{{"manifest_version":2,"id":"{id}","version":"0.1.0","min_kernel_version":"0.1.0","display_name":"X","entrypoint":{{"command":"bin/tool"}},"exposes_tools":[{{"name":"{tool}"}}]}}"#
            ))
            .unwrap()
        };
        let plugins = PluginRegistry::from_manifests([
            (manifest("ab-c", "d"), None),
            (manifest("ab", "c_d"), None),
        ]);
        let registry = build_default_registry();
        for running in ["ab-c", "ab"] {
            let rows = entries(
                vec![tool("plugin_ab_c_d".into())],
                &registry,
                Some(&plugins),
                &BTreeSet::from([running.to_string()]),
            )
            .unwrap();
            let row = rows
                .iter()
                .find(|entry| entry.tool.name == "plugin_ab_c_d")
                .unwrap();
            assert_eq!(
                row.plugin,
                Some(PluginOwner {
                    id: running.into(),
                    kind: None
                })
            );
        }
    }

    #[test]
    fn tool_parse_errors_list_the_actions() {
        for argv in [&["tool"][..], &["tool", "names", "--all"][..]] {
            let argv: Vec<String> = argv.iter().map(|arg| arg.to_string()).collect();
            let message = parse(&argv).unwrap_err();
            assert!(
                message.contains("neige tool ls") && message.contains("neige tool describe"),
                "{message}"
            );
        }
    }
}
