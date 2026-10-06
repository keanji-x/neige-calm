//! `neige tool ls|describe` (#2003 §4.5): a literal, bounded lookup over every tool this session
//! may call (#2289 D2), each with the surfaces that serve it, whether its `tools/list` shows it and
//! the plugin that serves it (#2227). The set is [`SessionCatalog`]'s; this module only selects
//! and renders it.
use super::commands::{JSON, cli_spelling};
use super::{CliExit, Output};
use crate::mcp_server::{
    registry::{AppContext, ConnectionIdentity, ToolDescriptor, ToolRegistry},
    transport::{PluginOwner, SessionCatalog, card_bound_catalog, tool_owner},
};
use crate::model::CardRole;
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

/// One catalog entry: the descriptor and whether this session's `tools/list` shows it.
#[derive(Clone)]
struct Entry {
    tool: ToolDescriptor,
    listed: bool,
}

/// Why `select` refused, rendered by [`run`] with its exit and detail.
#[derive(Debug, PartialEq)]
enum Refusal {
    /// Unknown, outside the Track's plugin scope, not running, or over a byte limit.
    Catalog(String),
    /// Served, but not to this role: the tool's declared `roles`.
    Role {
        name: String,
        roles: &'static [CardRole],
        role: CardRole,
    },
}

impl From<String> for Refusal {
    fn from(message: String) -> Self {
        Self::Catalog(message)
    }
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

/// The session's callable tools by name, each marked whether its `tools/list` shows it.
fn entries(catalog: &SessionCatalog) -> Vec<Entry> {
    let listed: BTreeSet<&str> = catalog.listed().map(|tool| tool.name.as_str()).collect();
    let mut entries: Vec<Entry> = catalog
        .callable()
        .iter()
        .map(|tool| Entry {
            listed: listed.contains(tool.name.as_str()),
            tool: tool.clone(),
        })
        .collect();
    entries.sort_by(|a, b| a.tool.name.cmp(&b.tool.name));
    entries
}

/// Who serves a selected row: resolved only for the rows `select` returns (#2287), so a plugin
/// that changes while listing fails only the rows it serves.
type Owner<'a> = &'a dyn Fn(&str) -> Result<Option<PluginOwner>, String>;

/// `plugin` is the serving plugin's id, `kind` the kind its manifest declares; both null for a
/// kernel tool.
fn ownership(plugin: &Option<PluginOwner>) -> (Value, Value) {
    match plugin {
        Some(owner) => (json!(owner.id), json!(owner.kind)),
        None => (Value::Null, Value::Null),
    }
}

/// Every tool is served over MCP; a `COMMANDS` row adds the CLI.
fn surfaces(name: &str) -> Value {
    match cli_spelling(name) {
        Some(_) => json!(["mcp", "cli"]),
        None => json!(["mcp"]),
    }
}

fn row(entry: &Entry, owner: Owner<'_>) -> Result<Value, String> {
    let (plugin, kind) = ownership(&owner(&entry.tool.name)?);
    Ok(json!({
        "name": entry.tool.name,
        "surfaces": surfaces(&entry.tool.name),
        "cli": cli_spelling(&entry.tool.name),
        "listed": entry.listed,
        "plugin": plugin,
        "kind": kind,
    }))
}

fn select(
    query: Query,
    entries: Vec<Entry>,
    refusing: impl Fn(&str) -> Option<&'static [CardRole]>,
    role: CardRole,
    owner: Owner<'_>,
) -> Result<Value, Refusal> {
    match query {
        Query::Describe { name } => {
            let Some(entry) = entries.into_iter().find(|entry| entry.tool.name == name) else {
                return Err(match refusing(&name) {
                    Some(roles) => Refusal::Role { name, roles, role },
                    // Unknown, or a plugin tool outside the Track's scope or not running: the
                    // same refusal, so describe is no existence oracle for plugin scope.
                    None => Refusal::Catalog(format!(
                        "no tool `{name}` in this session; `neige tool ls --all` lists them"
                    )),
                });
            };
            let mut value = entry.tool.clone().into_mcp_value();
            let object = value.as_object_mut().expect("a descriptor is an object");
            object.insert("surfaces".into(), surfaces(&entry.tool.name));
            object.insert("cli".into(), json!(cli_spelling(&entry.tool.name)));
            object.insert("listed".into(), json!(entry.listed));
            let (plugin, kind) = ownership(&owner(&entry.tool.name)?);
            object.insert("plugin".into(), plugin);
            object.insert("kind".into(), kind);
            if value.to_string().len() > DETAIL_MAX_BYTES {
                return Err(Refusal::Catalog(
                    "tool declaration exceeds the lookup byte limit; use the client's exact-name tool loading"
                        .into(),
                ));
            }
            Ok(value)
        }
        Query::List { prefix, cursor } => {
            if cursor
                .as_ref()
                .is_some_and(|name| !name.starts_with(&prefix))
            {
                return Err(Refusal::Catalog(
                    "--cursor must be a next_cursor of the requested prefix".into(),
                ));
            }
            let matching: Vec<&Entry> = entries
                .iter()
                .filter(|entry| {
                    entry.tool.name.starts_with(&prefix)
                        && cursor.as_ref().is_none_or(|last| entry.tool.name > *last)
                })
                .collect();
            let rows: Vec<(String, Value)> = matching
                .iter()
                .take(PAGE_SIZE)
                .map(|entry| Ok((entry.tool.name.clone(), row(entry, owner)?)))
                .collect::<Result<_, String>>()?;
            let mut count = rows.len();
            loop {
                let next = (count < matching.len())
                    .then(|| count.checked_sub(1).map(|index| rows[index].0.clone()))
                    .flatten();
                let page: Vec<&Value> = rows[..count].iter().map(|(_, row)| row).collect();
                let value = json!({"tools": page, "next_cursor": next});
                if value.to_string().len() <= LIST_MAX_BYTES {
                    return Ok(value);
                }
                if count <= 1 {
                    return Err(Refusal::Catalog(
                        "one tool row exceeds the lookup byte limit".into(),
                    ));
                }
                count -= 1;
            }
        }
    }
}

/// Text output: one `name  mcp|mcp+cli  cli-or-—  listed|hidden  kernel|plugin:<id>  kind-or-—`
/// row per tool, or the describe JSON, then the footer.
fn text(value: &Value) -> String {
    let mut out = match value["tools"].as_array() {
        Some(rows) => {
            let mut out: String = rows
                .iter()
                .map(|row| {
                    format!(
                        "{}  {}  {}  {}  {}  {}\n",
                        row["name"].as_str().expect("rows carry a name"),
                        row["surfaces"]
                            .as_array()
                            .expect("rows carry their surfaces")
                            .iter()
                            .map(|surface| surface.as_str().expect("a surface is a word"))
                            .collect::<Vec<_>>()
                            .join("+"),
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
    let ConnectionIdentity::CardBound(bound) = identity else {
        return Output::error(
            CliExit::Failed,
            json_mode,
            "tool lookup requires an active card-bound session".into(),
            json!({"kind":"identity"}),
        );
    };
    let catalog = match card_bound_catalog(ctx, registry, bound).await {
        Ok(catalog) => catalog,
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
    let plugins: Option<&PluginRegistry> = host.map(|host| host.registry().as_ref());
    let owner =
        |name: &str| tool_owner(registry, plugins, &running_ids, name).map_err(|e| e.message);
    match select(
        query,
        entries(&catalog),
        |name| catalog.roles_refusing(name),
        catalog.role,
        &owner,
    ) {
        Err(Refusal::Catalog(message)) => Output::error(
            CliExit::Failed,
            json_mode,
            message,
            json!({"kind":"catalog"}),
        ),
        Err(Refusal::Role { name, roles, role }) => Output::error(
            CliExit::Failed,
            json_mode,
            format!("`{name}` is for roles {roles:?}; this session is {role:?}"),
            json!({
                "kind": "role",
                "roles": roles.iter().map(|role| format!("{role:?}")).collect::<Vec<_>>(),
                "role": format!("{role:?}"),
            }),
        ),
        Ok(value) if json_mode => Output::success(format!("{value}\n")),
        Ok(value) => Output::success(text(&value)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_server::build_default_registry;
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

    fn entry(name: &str, listed: bool) -> Entry {
        Entry {
            tool: tool(name.into()),
            listed,
        }
    }

    fn listed(names: &[String]) -> Vec<Entry> {
        names.iter().map(|name| entry(name, true)).collect()
    }

    fn gitforge(_: &str) -> Result<Option<PluginOwner>, String> {
        Ok(Some(PluginOwner {
            id: "gitforge".into(),
            kind: Some(ToolKind::ForgeAction),
        }))
    }

    fn kernel(_: &str) -> Result<Option<PluginOwner>, String> {
        Ok(None)
    }

    fn nobody_refuses(_: &str) -> Option<&'static [CardRole]> {
        None
    }

    fn select_as_planner(
        query: Query,
        entries: Vec<Entry>,
        owner: Owner<'_>,
    ) -> Result<Value, Refusal> {
        select(query, entries, nobody_refuses, CardRole::Planner, owner)
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
            let page = select_as_planner(
                Query::List {
                    prefix: "plugin_".into(),
                    cursor: cursor.clone(),
                },
                entries.clone(),
                &gitforge,
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
            let page = select_as_planner(query, entries.clone(), &gitforge).unwrap();
            for name in page_names(&page) {
                let (query, _) = parse(&[
                    "tool".into(),
                    "describe".into(),
                    "--name".into(),
                    name.clone(),
                ])
                .unwrap();
                assert_eq!(
                    select_as_planner(query, entries.clone(), &gitforge).unwrap()["name"],
                    name
                );
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
        assert!(matches!(
            select_as_planner(
                Query::Describe {
                    name: large.name.clone()
                },
                vec![Entry {
                    tool: large,
                    listed: true,
                }],
                &kernel,
            )
            .unwrap_err(),
            Refusal::Catalog(message) if message.contains("byte limit")
        ));
        let details = select_as_planner(
            Query::Describe {
                name: "neige_small_tool".into(),
            },
            listed(&["neige_small_tool".into()]),
            &gitforge,
        )
        .unwrap();
        assert!(details.get("annotations").is_none());
        assert_eq!(details["inputSchema"], json!({"type":"object"}));
        assert_eq!(
            (&details["surfaces"], &details["cli"], &details["listed"]),
            (&json!(["mcp"]), &Value::Null, &json!(true))
        );
        assert_eq!(
            (&details["plugin"], &details["kind"]),
            (&json!("gitforge"), &json!("forge-action"))
        );
    }

    /// #2289 D2: a row carries its surfaces, `mcp+cli` when a command serves it, with the derived
    /// command; `listed` is the entry's own mark, never derived from the command.
    #[test]
    fn rows_carry_surfaces_and_the_derived_command() {
        let entries = vec![
            entry("neige_track_cat", false),
            entry("neige_track_close", true),
            entry("neige_track_rename", true),
        ];
        let page = select_as_planner(
            Query::List {
                prefix: "neige_track_".into(),
                cursor: None,
            },
            entries.clone(),
            &kernel,
        )
        .unwrap();
        let rows = page["tools"].as_array().unwrap();
        assert_eq!(
            rows,
            &vec![
                json!({"name":"neige_track_cat","surfaces":["mcp","cli"],"cli":"neige track cat","listed":false,"plugin":null,"kind":null}),
                json!({"name":"neige_track_close","surfaces":["mcp","cli"],"cli":"neige track close","listed":true,"plugin":null,"kind":null}),
                json!({"name":"neige_track_rename","surfaces":["mcp"],"cli":null,"listed":true,"plugin":null,"kind":null}),
            ],
            "{page}"
        );
        assert_eq!(
            text(&page),
            format!(
                "neige_track_cat  mcp+cli  neige track cat  hidden  kernel  —\n\
                 neige_track_close  mcp+cli  neige track close  listed  kernel  —\n\
                 neige_track_rename  mcp  —  listed  kernel  —\n\
                 next_cursor: null\n{FOOTER}\n"
            )
        );
        let described = select_as_planner(
            Query::Describe {
                name: "neige_track_cat".into(),
            },
            entries,
            &kernel,
        )
        .unwrap();
        assert_eq!(
            (
                &described["surfaces"],
                &described["cli"],
                &described["listed"]
            ),
            (
                &json!(["mcp", "cli"]),
                &json!("neige track cat"),
                &json!(false)
            )
        );
    }

    /// #2289 D3: describe tells a tool served to other roles from one this session cannot reach.
    #[test]
    fn describe_names_the_declared_roles_of_a_tool_served_to_others() {
        let refusing = |name: &str| (name == "neige_admin_gc").then_some(&[CardRole::Planner][..]);
        let describe = |name: &str| {
            select(
                Query::Describe { name: name.into() },
                vec![entry("neige_report_read", true)],
                refusing,
                CardRole::Assistant,
                &kernel,
            )
        };
        assert_eq!(
            describe("neige_admin_gc").unwrap_err(),
            Refusal::Role {
                name: "neige_admin_gc".into(),
                roles: &[CardRole::Planner],
                role: CardRole::Assistant,
            }
        );
        assert!(matches!(
            describe("neige_nope_x").unwrap_err(),
            Refusal::Catalog(message) if message.starts_with("no tool `neige_nope_x`")
        ));
        assert_eq!(
            describe("neige_report_read").unwrap()["listed"],
            json!(true)
        );
    }

    /// #2227: every row names the plugin that serves it. A built-in native
    /// (`plugin_gitforge_publish`) answers to its compiled owner with no kind; a manifest tool
    /// carries its declared kind; a name nobody serves is refused rather than shown as a kernel
    /// tool, and only when a returned row needs it (#2287).
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
        let running: BTreeSet<String> = ["gitforge", "calendar", "invest"].map(String::from).into();
        let owner = |name: &str| {
            tool_owner(&registry, Some(&plugins), &running, name).map_err(|e| e.message)
        };
        let entries: Vec<Entry> = [
            "plugin_gitforge_publish",
            "plugin_calendar_add",
            "plugin_gitforge_git_commit",
            "plugin_invest_instrument_add",
            "neige_track_rename",
            "plugin_ghost_tool",
        ]
        .map(|name| entry(name, true))
        .to_vec();
        let list = |prefix: &str| {
            select_as_planner(
                Query::List {
                    prefix: prefix.into(),
                    cursor: None,
                },
                entries.clone(),
                &owner,
            )
        };
        let mut rows = Vec::new();
        for prefix in [
            "plugin_gitforge_",
            "plugin_calendar_",
            "plugin_invest_",
            "neige_",
        ] {
            rows.extend(list(prefix).unwrap()["tools"].as_array().unwrap().clone());
        }
        let owner_of = |name: &str| {
            let row = rows.iter().find(|row| row["name"] == name).unwrap();
            (row["plugin"].clone(), row["kind"].clone())
        };
        assert_eq!(
            owner_of("plugin_gitforge_publish"),
            (json!("gitforge"), Value::Null)
        );
        assert_eq!(
            owner_of("plugin_calendar_add"),
            (json!("calendar"), Value::Null)
        );
        assert_eq!(
            owner_of("plugin_gitforge_git_commit"),
            (json!("gitforge"), json!("forge-action"))
        );
        assert_eq!(
            owner_of("plugin_invest_instrument_add"),
            (json!("invest"), Value::Null)
        );
        assert_eq!(owner_of("neige_track_rename"), (Value::Null, Value::Null));
        let page = json!({"tools": rows, "next_cursor": null});
        assert!(
            text(&page).contains("plugin_gitforge_publish  mcp  —  listed  plugin:gitforge  —\n"),
            "{}",
            text(&page)
        );
        assert!(text(&page).contains(
            "plugin_gitforge_git_commit  mcp  —  listed  plugin:gitforge  forge-action\n"
        ));
        let Refusal::Catalog(ghost) = list("plugin_ghost_").unwrap_err() else {
            panic!("a name nobody serves is a catalog refusal");
        };
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
            let running_ids = BTreeSet::from([running.to_string()]);
            let owner = |name: &str| {
                tool_owner(&registry, Some(&plugins), &running_ids, name).map_err(|e| e.message)
            };
            let details = select_as_planner(
                Query::Describe {
                    name: "plugin_ab_c_d".into(),
                },
                vec![entry("plugin_ab_c_d", true)],
                &owner,
            )
            .unwrap();
            assert_eq!(details["plugin"], json!(running));
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
