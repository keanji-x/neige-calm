//! `neige tool ls|describe` (#2003 §4.5): a literal, bounded lookup over this session's
//! `tools/list` set plus every tool a `neige` command calls. Listing is not a grant.
use super::commands::{JSON, cli_spelling, command_for_tool};
use super::{CliExit, Output};
use crate::mcp_server::{
    registry::{AppContext, ConnectionIdentity, ToolDescriptor, ToolRegistry},
    transport::tool_descriptors_for_connection,
};
use serde_json::{Value, json};
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
        after: Option<String>,
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

const LS_USAGE: &str = "neige tool ls (--prefix PREFIX | --all) [--after NAME]";
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
            let mut after = None;
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
                    "--after"
                        if after.is_none()
                            && args.get(index + 1).is_some_and(|value| tool_name(value)) =>
                    {
                        after = Some(args[index + 1].to_string());
                        index += 2;
                    }
                    _ => {
                        return Err(format!(
                            "use `{LS_USAGE}`: a literal --prefix or explicit --all, with optional --after"
                        ));
                    }
                }
            }
            prefix
                .map(|prefix| (Query::List { prefix, after }, json))
                .ok_or_else(|| format!("tool ls requires --prefix or --all; use `{LS_USAGE}`"))
        }
        Some(action) => Err(format!(
            "unknown action `{action}` for `neige tool`; {}",
            choices()
        )),
        None => Err(format!("`neige tool` needs an action; {}", choices())),
    }
}

/// The session's `tools/list` set plus every CLI-covered tool, by name.
fn entries(listed: Vec<ToolDescriptor>, registry: &ToolRegistry) -> Vec<Entry> {
    let mut entries: Vec<Entry> = listed
        .into_iter()
        .map(|tool| Entry { tool, listed: true })
        .collect();
    for tool in registry.descriptors() {
        if command_for_tool(&tool.name).is_some()
            && !entries.iter().any(|entry| entry.tool.name == tool.name)
        {
            entries.push(Entry {
                tool,
                listed: false,
            });
        }
    }
    entries.sort_by(|a, b| a.tool.name.cmp(&b.tool.name));
    entries
}

fn row(entry: &Entry) -> Value {
    json!({ "name": entry.tool.name, "cli": cli_spelling(&entry.tool.name), "listed": entry.listed })
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
            if value.to_string().len() > DETAIL_MAX_BYTES {
                return Err(
                    "tool declaration exceeds the lookup byte limit; use the client's exact-name tool loading"
                        .into(),
                );
            }
            Ok(value)
        }
        Query::List { prefix, after } => {
            if after
                .as_ref()
                .is_some_and(|name| !name.starts_with(&prefix))
            {
                return Err("--after must belong to the requested prefix".into());
            }
            let rows: Vec<(String, Value)> = entries
                .iter()
                .filter(|entry| {
                    entry.tool.name.starts_with(&prefix)
                        && after.as_ref().is_none_or(|last| entry.tool.name > *last)
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

/// Text output: one `name  cli-or-—  listed|hidden` row per tool, or the describe JSON, then the footer.
fn text(value: &Value) -> String {
    let mut out = match value["tools"].as_array() {
        Some(rows) => {
            let mut out: String = rows
                .iter()
                .map(|row| {
                    format!(
                        "{}  {}  {}\n",
                        row["name"].as_str().expect("rows carry a name"),
                        row["cli"].as_str().unwrap_or("—"),
                        if row["listed"] == json!(true) {
                            "listed"
                        } else {
                            "hidden"
                        }
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
    match select(query, entries(listed, registry)) {
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

    fn tool(name: String) -> ToolDescriptor {
        ToolDescriptor {
            name,
            description: "Read only".into(),
            input_schema: json!({"type":"object"}),
            annotations: None,
            visible_to_roles: &[CardRole::Planner],
        }
    }

    fn listed(names: &[String]) -> Vec<Entry> {
        names
            .iter()
            .cloned()
            .map(|name| Entry {
                tool: tool(name),
                listed: true,
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
        let mut after = None;
        let mut seen = Vec::new();
        loop {
            let page = select(
                Query::List {
                    prefix: "plugin_".into(),
                    after: after.clone(),
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
                    assert_ne!(after.as_deref(), Some(next));
                    after = Some(next.into());
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
            argv.extend(["--after".into(), cursor.into()]);
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
                    listed: true
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
    }

    /// #2003 §4.5: a hidden CLI-covered tool is still discoverable, marked `listed: false` with its
    /// derived command; a listed tool without a command has `cli: null`.
    #[test]
    fn tool_list_includes_cli_covered_hidden_tools() {
        let registry = build_default_registry();
        let planner = registry.descriptors_for_role(CardRole::Planner);
        assert!(
            !planner.iter().any(|tool| tool.name == "neige_track_cat"),
            "precondition: track.cat is hidden from the Planner's tools/list"
        );
        let page = select(
            Query::List {
                prefix: "neige_track_".into(),
                after: None,
            },
            entries(planner, &registry),
        )
        .unwrap();
        let rows = page["tools"].as_array().unwrap();
        assert!(
            rows.contains(
                &json!({"name":"neige_track_cat","cli":"neige track cat","listed":false})
            ),
            "{page}"
        );
        assert!(
            rows.contains(
                &json!({"name":"neige_track_close","cli":"neige track close","listed":true})
            ),
            "{page}"
        );
        assert!(
            rows.contains(&json!({"name":"neige_track_rename","cli":null,"listed":true})),
            "{page}"
        );
        let described = select(
            Query::Describe {
                name: "neige_track_cat".into(),
            },
            entries(registry.descriptors_for_role(CardRole::Planner), &registry),
        )
        .unwrap();
        assert_eq!(
            (&described["cli"], &described["listed"]),
            (&json!("neige track cat"), &json!(false))
        );
        assert!(text(&page).ends_with(&format!("{FOOTER}\n")));
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
