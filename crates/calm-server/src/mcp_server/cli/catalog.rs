//! Literal, bounded lookup over the same authorized catalog as `tools/list`.
use super::{CliExit, Output};
use crate::mcp_server::{
    registry::{AppContext, ConnectionIdentity, ToolDescriptor, ToolRegistry},
    transport::tool_descriptors_for_connection,
};
use serde_json::{Value, json};
use std::sync::Arc;

pub(super) const COMMAND_NAME: &str = "tools";

const PAGE_SIZE: usize = 20;
const NAMES_MAX_BYTES: usize = 4096;
const DETAIL_MAX_BYTES: usize = 8192;
const INPUT_MAX_BYTES: usize = 256;

enum Query {
    Names {
        prefix: String,
        after: Option<String>,
    },
    Describe {
        name: String,
    },
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

fn parse(argv: &[String]) -> Result<(Query, bool), &'static str> {
    let json = argv.iter().any(|arg| arg == "--json");
    let args: Vec<&str> = argv
        .iter()
        .map(String::as_str)
        .filter(|arg| *arg != "--json")
        .collect();
    let Some(command) = args.get(1) else {
        return Err("choose tools names or tools describe");
    };
    match *command {
        "describe" if args.len() == 4 && args[2] == "--name" && tool_name(args[3]) => Ok((
            Query::Describe {
                name: args[3].into(),
            },
            json,
        )),
        "names" => {
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
                        return Err(
                            "use a literal --prefix or explicit --all, with optional --after",
                        );
                    }
                }
            }
            prefix
                .map(|prefix| (Query::Names { prefix, after }, json))
                .ok_or("tools names requires --prefix or --all")
        }
        _ => Err(
            "use tools names (--prefix PREFIX | --all) [--after NAME], or tools describe --name NAME",
        ),
    }
}

fn select(query: Query, mut tools: Vec<ToolDescriptor>) -> Result<Value, &'static str> {
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    match query {
        Query::Describe { name } => {
            let tool = tools
                .into_iter()
                .find(|tool| tool.name == name)
                .ok_or("tool is not visible to this session")?;
            let value = tool.into_mcp_value();
            if value.to_string().len() > DETAIL_MAX_BYTES {
                return Err(
                    "tool declaration exceeds the lookup byte limit; use the client's exact-name tool loading",
                );
            }
            Ok(value)
        }
        Query::Names { prefix, after } => {
            if after
                .as_ref()
                .is_some_and(|name| !name.starts_with(&prefix))
            {
                return Err("--after must belong to the requested prefix");
            }
            let names: Vec<String> = tools
                .into_iter()
                .map(|tool| tool.name)
                .filter(|name| {
                    name.starts_with(&prefix) && after.as_ref().is_none_or(|last| name > last)
                })
                .collect();
            let mut count = names.len().min(PAGE_SIZE);
            let value = loop {
                let next = if count < names.len() {
                    count
                        .checked_sub(1)
                        .and_then(|index| names.get(index))
                        .cloned()
                } else {
                    None
                };
                let value = json!({"names": &names[..count], "next_cursor": next});
                if value.to_string().len() <= NAMES_MAX_BYTES {
                    break value;
                }
                if count <= 1 {
                    return Err("one tool name exceeds the lookup byte limit");
                }
                count -= 1;
            };
            Ok(value)
        }
    }
}

pub(super) async fn run(
    ctx: &Arc<AppContext>,
    registry: &ToolRegistry,
    identity: &ConnectionIdentity,
    argv: &[String],
) -> Output {
    let json_mode = argv.iter().any(|arg| arg == "--json");
    let (query, json_mode) = match parse(argv) {
        Ok(query) => query,
        Err(message) => return Output::usage(message.into(), json_mode, Some("tools")),
    };
    if !matches!(identity, ConnectionIdentity::CardBound(_)) {
        return Output::error(
            CliExit::Failed,
            json_mode,
            "tool lookup requires an active card-bound session".into(),
            json!({"kind":"identity"}),
        );
    }
    let tools = match tool_descriptors_for_connection(ctx, registry, identity, None).await {
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
    match select(query, tools) {
        Err(message) => Output::error(
            CliExit::Failed,
            json_mode,
            message.into(),
            json!({"kind":"catalog"}),
        ),
        Ok(value) => {
            if !json_mode && let Some(names) = value["names"].as_array() {
                let mut out = names
                    .iter()
                    .map(|name| format!("{}\n", name.as_str().unwrap()))
                    .collect::<String>();
                out.push_str(&format!("next_cursor: {}\n", value["next_cursor"]));
                return Output::success(out);
            }
            Output::success(format!("{value}\n"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn names_pages_cover_long_names_without_silent_truncation() {
        let names: Vec<String> = (0..43)
            .map(|index| format!("plugin.{}.tool{index:02}", "x".repeat(180)))
            .collect();
        let tools: Vec<ToolDescriptor> = names.iter().cloned().map(tool).collect();
        let mut after = None;
        let mut seen = Vec::new();
        loop {
            let page = select(
                Query::Names {
                    prefix: "plugin.".into(),
                    after: after.clone(),
                },
                tools.clone(),
            )
            .unwrap();
            assert!(page.to_string().len() <= NAMES_MAX_BYTES);
            let batch = page["names"].as_array().unwrap();
            assert!(!batch.is_empty() && batch.len() <= PAGE_SIZE);
            seen.extend(batch.iter().map(|name| name.as_str().unwrap().to_string()));
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
            .map(|index| format!("plugin.demo_读取/{index:02}:{}", "x".repeat(300)))
            .collect();
        let tools: Vec<ToolDescriptor> = names.iter().cloned().map(tool).collect();
        let mut argv = vec![
            "tools".into(),
            "names".into(),
            "--prefix".into(),
            "plugin.".into(),
        ];
        let mut seen = Vec::new();
        loop {
            let (query, _) = parse(&argv).unwrap();
            let page = select(query, tools.clone()).unwrap();
            for name in page["names"].as_array().unwrap() {
                let name = name.as_str().unwrap();
                let (query, _) = parse(&[
                    "tools".into(),
                    "describe".into(),
                    "--name".into(),
                    name.into(),
                ])
                .unwrap();
                assert_eq!(select(query, tools.clone()).unwrap()["name"], name);
                seen.push(name.to_string());
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
        let mut large = tool("neige.large".into());
        large.description = "x".repeat(DETAIL_MAX_BYTES);
        assert!(
            select(
                Query::Describe {
                    name: large.name.clone()
                },
                vec![large]
            )
            .unwrap_err()
            .contains("byte limit")
        );
        let plain = tool("neige.small".into());
        let details = select(
            Query::Describe {
                name: plain.name.clone(),
            },
            vec![plain],
        )
        .unwrap();
        assert!(details.get("annotations").is_none());
        assert_eq!(details["inputSchema"], json!({"type":"object"}));
    }
}
