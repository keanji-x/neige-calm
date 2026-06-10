//! `neige` — terminal CLI for read-only wave file MCP tools.
//!
//! The CLI is intentionally tiny. It inherits the per-card MCP socket and raw
//! token from the terminal environment, initializes the existing kernel MCP
//! server with the token under `params._meta["dev.neige/auth"].token`, then
//! performs one `tools/call` for wave reads or worker task reports.
//! `NEIGE_MCP_DAEMON_TOKEN` is for the stdio shim only; the CLI requires
//! `NEIGE_MCP_TOKEN` because its tool calls do not carry thread metadata.

use std::env;
use std::io::{self, Write};
use std::process::ExitCode;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

const ENV_SOCKET: &str = "NEIGE_MCP_SOCKET";
const ENV_TOKEN: &str = "NEIGE_MCP_TOKEN";
const TOOL_WAVE_LS: &str = "calm.wave.ls";
const TOOL_WAVE_CAT: &str = "calm.wave.cat";
const TOOL_WAVE_STATE: &str = "calm.wave.state";
const TOOL_TASK_COMPLETE: &str = "calm.task.complete";
const TOOL_TASK_FAIL: &str = "calm.task.fail";
const PROTOCOL_VERSION: &str = "2024-11-05";

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.first().map(String::as_str) == Some("--version") {
        println!("neige {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }

    let cli = match Cli::parse(args) {
        Ok(cli) => cli,
        Err(err) => {
            emit_error(&err);
            return ExitCode::from(err.exit_code);
        }
    };

    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            emit_error(&err);
            ExitCode::from(err.exit_code)
        }
    }
}

async fn run(cli: Cli) -> Result<(), AppError> {
    let socket = env::var(ENV_SOCKET)
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::missing_env(ENV_SOCKET, cli.json_errors()))?;
    let token = env::var(ENV_TOKEN)
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::missing_env(ENV_TOKEN, cli.json_errors()))?;

    let raw = call_wave_tool(&socket, &token, &cli).await?;
    match cli.command {
        Command::Ls { json_output, .. } => render_ls(&raw, json_output, cli.json_errors()),
        Command::Cat { .. } => render_cat(&raw, cli.json_errors()),
        Command::State { json_output } => render_state(&raw, json_output, cli.json_errors()),
        Command::TaskCompleted { .. } | Command::TaskFailed { .. } => {
            let serialized = serde_json::to_string(&raw).map_err(|e| {
                AppError::new(
                    format!("serialize task report JSON: {e}"),
                    4,
                    cli.json_errors(),
                    json!({ "kind": "shape", "message": e.to_string() }),
                )
            })?;
            println!("{serialized}");
            Ok(())
        }
    }
}

async fn call_wave_tool(socket: &str, token: &str, cli: &Cli) -> Result<Value, AppError> {
    let stream = UnixStream::connect(socket).await.map_err(|e| {
        AppError::new(
            format!("connect {socket}: {e}"),
            3,
            cli.json_errors(),
            json!({ "kind": "connect", "socket": socket, "message": e.to_string() }),
        )
    })?;
    let (rd, mut wr) = stream.into_split();
    let mut reader = BufReader::new(rd);

    let init = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {
                "name": "neige",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "_meta": {
                "dev.neige/auth": {
                    "token": token,
                }
            }
        }
    });
    write_frame(&mut wr, &init, cli.json_errors()).await?;
    let init_response = read_response(&mut reader, cli.json_errors()).await?;
    if let Some(error) = init_response.get("error") {
        return Err(AppError::rpc(
            "initialize",
            error.clone(),
            cli.json_errors(),
        ));
    }

    let (name, arguments) = match &cli.command {
        Command::Ls { path, .. } => (TOOL_WAVE_LS, json!({ "path": path })),
        Command::Cat { path, .. } => (TOOL_WAVE_CAT, json!({ "path": path })),
        Command::State { .. } => (TOOL_WAVE_STATE, json!({})),
        Command::TaskCompleted {
            idempotency_key,
            result,
            artifacts,
            ..
        } => (
            TOOL_TASK_COMPLETE,
            json!({
                "idempotency_key": idempotency_key,
                "result": result.clone().unwrap_or(Value::Null),
                "artifacts": artifacts,
            }),
        ),
        Command::TaskFailed {
            idempotency_key,
            reason,
            ..
        } => (
            TOOL_TASK_FAIL,
            json!({
                "idempotency_key": idempotency_key,
                "reason": reason,
            }),
        ),
    };
    let call = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": name,
            "arguments": arguments,
        }
    });
    write_frame(&mut wr, &call, cli.json_errors()).await?;
    let call_response = read_response(&mut reader, cli.json_errors()).await?;
    if let Some(error) = call_response.get("error") {
        return Err(AppError::rpc(name, error.clone(), cli.json_errors()));
    }
    let result = call_response.get("result").ok_or_else(|| {
        AppError::new(
            "protocol error: response missing result",
            4,
            cli.json_errors(),
            json!({ "kind": "protocol", "message": "response missing result" }),
        )
    })?;
    if result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(AppError::new(
            format!("{name}: tool returned isError=true"),
            4,
            cli.json_errors(),
            json!({ "kind": "tool", "tool": name, "result": result }),
        ));
    }
    result
        .get("structuredContent")
        .cloned()
        .or_else(|| {
            result
                .get("content")
                .and_then(Value::as_array)
                .and_then(|blocks| blocks.first())
                .and_then(|block| block.get("text"))
                .and_then(Value::as_str)
                .and_then(|text| serde_json::from_str(text).ok())
        })
        .ok_or_else(|| {
            AppError::new(
                "protocol error: tool response missing structuredContent",
                4,
                cli.json_errors(),
                json!({ "kind": "protocol", "message": "tool response missing structuredContent" }),
            )
        })
}

async fn write_frame(
    wr: &mut tokio::net::unix::OwnedWriteHalf,
    value: &Value,
    json_error: bool,
) -> Result<(), AppError> {
    let mut line = serde_json::to_vec(value).map_err(|e| {
        AppError::new(
            format!("serialize JSON-RPC frame: {e}"),
            4,
            json_error,
            json!({ "kind": "protocol", "message": e.to_string() }),
        )
    })?;
    line.push(b'\n');
    wr.write_all(&line).await.map_err(|e| {
        AppError::new(
            format!("write JSON-RPC frame: {e}"),
            4,
            json_error,
            json!({ "kind": "io", "message": e.to_string() }),
        )
    })?;
    wr.flush().await.map_err(|e| {
        AppError::new(
            format!("flush JSON-RPC frame: {e}"),
            4,
            json_error,
            json!({ "kind": "io", "message": e.to_string() }),
        )
    })
}

async fn read_response(
    reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
    json_error: bool,
) -> Result<Value, AppError> {
    let mut line = String::new();
    let n = reader.read_line(&mut line).await.map_err(|e| {
        AppError::new(
            format!("read JSON-RPC frame: {e}"),
            4,
            json_error,
            json!({ "kind": "io", "message": e.to_string() }),
        )
    })?;
    if n == 0 {
        return Err(AppError::new(
            "protocol error: server closed connection",
            4,
            json_error,
            json!({ "kind": "protocol", "message": "server closed connection" }),
        ));
    }
    serde_json::from_str(line.trim_end_matches(['\n', '\r'])).map_err(|e| {
        AppError::new(
            format!("parse JSON-RPC response: {e}"),
            4,
            json_error,
            json!({ "kind": "protocol", "message": e.to_string() }),
        )
    })
}

fn render_ls(value: &Value, json_output: bool, json_error: bool) -> Result<(), AppError> {
    let entries = value.as_array().ok_or_else(|| {
        AppError::new(
            "calm.wave.ls returned non-array structuredContent",
            4,
            json_error,
            json!({ "kind": "shape", "tool": TOOL_WAVE_LS, "value": value }),
        )
    })?;
    if json_output {
        println!(
            "{}",
            serde_json::to_string(entries).map_err(|e| {
                AppError::new(
                    format!("serialize ls JSON: {e}"),
                    4,
                    json_error,
                    json!({ "kind": "shape", "message": e.to_string() }),
                )
            })?
        );
        return Ok(());
    }

    let mut stdout = io::stdout();
    for entry in entries {
        let name = entry.get("name").and_then(Value::as_str).ok_or_else(|| {
            AppError::new(
                "calm.wave.ls entry missing string name",
                4,
                json_error,
                json!({ "kind": "shape", "tool": TOOL_WAVE_LS, "entry": entry }),
            )
        })?;
        let kind = entry.get("kind").and_then(Value::as_str).unwrap_or("file");
        let prefix = if kind == "dir" { 'd' } else { '-' };
        writeln!(stdout, "{prefix} {name}").map_err(|e| {
            AppError::new(
                format!("write stdout: {e}"),
                4,
                json_error,
                json!({ "kind": "io", "message": e.to_string() }),
            )
        })?;
    }
    Ok(())
}

fn render_cat(value: &Value, json_error: bool) -> Result<(), AppError> {
    let content = value
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            AppError::new(
                "calm.wave.cat returned content without a string content field",
                4,
                json_error,
                json!({ "kind": "shape", "tool": TOOL_WAVE_CAT, "value": value }),
            )
        })?;
    let content_type = value
        .get("content_type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if content_type == "application/json"
        && let Ok(parsed) = serde_json::from_str::<Value>(content)
    {
        println!(
            "{}",
            serde_json::to_string_pretty(&parsed).map_err(|e| {
                AppError::new(
                    format!("serialize cat JSON: {e}"),
                    4,
                    json_error,
                    json!({ "kind": "shape", "message": e.to_string() }),
                )
            })?
        );
        return Ok(());
    }
    print!("{content}");
    io::stdout().flush().map_err(|e| {
        AppError::new(
            format!("write stdout: {e}"),
            4,
            json_error,
            json!({ "kind": "io", "message": e.to_string() }),
        )
    })
}

fn render_state(value: &Value, json_output: bool, json_error: bool) -> Result<(), AppError> {
    if !value.is_object() {
        return Err(AppError::new(
            "calm.wave.state returned non-object structuredContent",
            4,
            json_error,
            json!({ "kind": "shape", "tool": TOOL_WAVE_STATE, "value": value }),
        ));
    }
    let serialized = if json_output {
        serde_json::to_string(value)
    } else {
        serde_json::to_string_pretty(value)
    }
    .map_err(|e| {
        AppError::new(
            format!("serialize state JSON: {e}"),
            4,
            json_error,
            json!({ "kind": "shape", "message": e.to_string() }),
        )
    })?;
    println!("{serialized}");
    Ok(())
}

fn emit_error(err: &AppError) {
    if err.json {
        let _ = writeln!(io::stderr(), "{}", err.structured);
    } else {
        let _ = writeln!(io::stderr(), "neige: {}", err.message);
    }
}

#[derive(Debug)]
struct Cli {
    command: Command,
}

impl Cli {
    fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, AppError> {
        let mut json = false;
        let mut iter = args.into_iter().peekable();
        while matches!(iter.peek().map(String::as_str), Some("--json")) {
            json = true;
            iter.next();
        }
        let command = iter.next().ok_or_else(|| {
            AppError::usage(
                "missing command; expected `ls`, `cat`, `state`, `task-completed`, or `task-failed`",
                json,
            )
        })?;
        match command.as_str() {
            "ls" => {
                let mut path: Option<String> = None;
                for arg in iter {
                    if arg == "--json" {
                        json = true;
                    } else if arg.starts_with('-') {
                        return Err(AppError::usage(format!("unknown option `{arg}`"), json));
                    } else if path.replace(arg).is_some() {
                        return Err(AppError::usage("ls accepts at most one path", json));
                    }
                }
                Ok(Self {
                    command: Command::Ls {
                        path: path.unwrap_or_else(|| "/".to_string()),
                        json_output: json,
                    },
                })
            }
            "cat" => {
                let mut path: Option<String> = None;
                for arg in iter {
                    if arg == "--json" {
                        json = true;
                    } else if arg.starts_with('-') {
                        return Err(AppError::usage(format!("unknown option `{arg}`"), json));
                    } else if path.replace(arg).is_some() {
                        return Err(AppError::usage("cat accepts exactly one path", json));
                    }
                }
                let path =
                    path.ok_or_else(|| AppError::usage("cat requires a path argument", json))?;
                Ok(Self {
                    command: Command::Cat {
                        path,
                        json_errors: json,
                    },
                })
            }
            "state" => {
                for arg in iter {
                    if arg == "--json" {
                        json = true;
                    } else if arg.starts_with('-') {
                        return Err(AppError::usage(format!("unknown option `{arg}`"), json));
                    } else {
                        return Err(AppError::usage("state takes no path argument", json));
                    }
                }
                Ok(Self {
                    command: Command::State { json_output: json },
                })
            }
            "task-completed" => {
                let mut idempotency_key: Option<String> = None;
                let mut result: Option<Value> = None;
                let mut artifacts: Vec<String> = Vec::new();
                while let Some(arg) = iter.next() {
                    match arg.as_str() {
                        "--json" => json = true,
                        "--idempotency-key" => {
                            let value = iter.next().ok_or_else(|| {
                                AppError::usage(
                                    "task-completed requires a value after --idempotency-key",
                                    json,
                                )
                            })?;
                            if value.is_empty() {
                                return Err(AppError::usage(
                                    "task-completed requires a non-empty --idempotency-key",
                                    json,
                                ));
                            }
                            if idempotency_key.replace(value).is_some() {
                                return Err(AppError::usage(
                                    "task-completed accepts --idempotency-key once",
                                    json,
                                ));
                            }
                        }
                        "--result" => {
                            let value = iter.next().ok_or_else(|| {
                                AppError::usage(
                                    "task-completed requires a value after --result",
                                    json,
                                )
                            })?;
                            // Try JSON first; fall back to a JSON string for
                            // plain text so the worker prompt's
                            // `<json-or-text>` contract matches the CLI.
                            let parsed = serde_json::from_str(&value)
                                .unwrap_or_else(|_| Value::String(value.clone()));
                            if result.replace(parsed).is_some() {
                                return Err(AppError::usage(
                                    "task-completed accepts --result once",
                                    json,
                                ));
                            }
                        }
                        "--artifact" => {
                            let value = iter.next().ok_or_else(|| {
                                AppError::usage(
                                    "task-completed requires a value after --artifact",
                                    json,
                                )
                            })?;
                            artifacts.push(value);
                        }
                        other if other.starts_with('-') => {
                            return Err(AppError::usage(format!("unknown option `{other}`"), json));
                        }
                        other => {
                            return Err(AppError::usage(
                                format!("unexpected argument `{other}`"),
                                json,
                            ));
                        }
                    }
                }
                let idempotency_key = idempotency_key.ok_or_else(|| {
                    AppError::usage("task-completed requires --idempotency-key", json)
                })?;
                Ok(Self {
                    command: Command::TaskCompleted {
                        idempotency_key,
                        result,
                        artifacts,
                        json_errors: json,
                    },
                })
            }
            "task-failed" => {
                let mut idempotency_key: Option<String> = None;
                let mut reason: Option<String> = None;
                while let Some(arg) = iter.next() {
                    match arg.as_str() {
                        "--json" => json = true,
                        "--idempotency-key" => {
                            let value = iter.next().ok_or_else(|| {
                                AppError::usage(
                                    "task-failed requires a value after --idempotency-key",
                                    json,
                                )
                            })?;
                            if value.is_empty() {
                                return Err(AppError::usage(
                                    "task-failed requires a non-empty --idempotency-key",
                                    json,
                                ));
                            }
                            if idempotency_key.replace(value).is_some() {
                                return Err(AppError::usage(
                                    "task-failed accepts --idempotency-key once",
                                    json,
                                ));
                            }
                        }
                        "--reason" => {
                            let value = iter.next().ok_or_else(|| {
                                AppError::usage("task-failed requires a value after --reason", json)
                            })?;
                            if value.is_empty() {
                                return Err(AppError::usage(
                                    "task-failed requires a non-empty --reason",
                                    json,
                                ));
                            }
                            if reason.replace(value).is_some() {
                                return Err(AppError::usage(
                                    "task-failed accepts --reason once",
                                    json,
                                ));
                            }
                        }
                        other if other.starts_with('-') => {
                            return Err(AppError::usage(format!("unknown option `{other}`"), json));
                        }
                        other => {
                            return Err(AppError::usage(
                                format!("unexpected argument `{other}`"),
                                json,
                            ));
                        }
                    }
                }
                let idempotency_key = idempotency_key.ok_or_else(|| {
                    AppError::usage("task-failed requires --idempotency-key", json)
                })?;
                let reason =
                    reason.ok_or_else(|| AppError::usage("task-failed requires --reason", json))?;
                Ok(Self {
                    command: Command::TaskFailed {
                        idempotency_key,
                        reason,
                        json_errors: json,
                    },
                })
            }
            other if other.starts_with('-') => {
                Err(AppError::usage(format!("unknown option `{other}`"), json))
            }
            other => Err(AppError::usage(format!("unknown command `{other}`"), json)),
        }
    }

    fn json_errors(&self) -> bool {
        match self.command {
            Command::Ls { json_output, .. } => json_output,
            Command::Cat { json_errors, .. } => json_errors,
            Command::State { json_output } => json_output,
            Command::TaskCompleted { json_errors, .. } => json_errors,
            Command::TaskFailed { json_errors, .. } => json_errors,
        }
    }
}

#[derive(Debug)]
enum Command {
    Ls {
        path: String,
        json_output: bool,
    },
    Cat {
        path: String,
        json_errors: bool,
    },
    State {
        json_output: bool,
    },
    TaskCompleted {
        idempotency_key: String,
        result: Option<Value>,
        artifacts: Vec<String>,
        json_errors: bool,
    },
    TaskFailed {
        idempotency_key: String,
        reason: String,
        json_errors: bool,
    },
}

#[derive(Debug)]
struct AppError {
    message: String,
    exit_code: u8,
    json: bool,
    structured: Value,
}

impl AppError {
    fn new(message: impl Into<String>, exit_code: u8, json: bool, detail: Value) -> Self {
        let message = message.into();
        Self {
            message: message.clone(),
            exit_code,
            json,
            structured: json!({
                "error": {
                    "message": message,
                    "detail": detail,
                }
            }),
        }
    }

    fn usage(message: impl Into<String>, json: bool) -> Self {
        Self::new(
            message,
            1,
            json,
            json!({
                "kind": "usage",
                "usage": "neige [--json] ls [path] | neige cat <path> | neige state | neige task-completed --idempotency-key K [--result <json-or-text>] [--artifact <path>]... | neige task-failed --idempotency-key K --reason <text>",
            }),
        )
    }

    fn missing_env(name: &str, json: bool) -> Self {
        Self::new(
            format!("missing {name} env var; run from a neige spec terminal"),
            2,
            json,
            json!({ "kind": "missing_env", "env": name }),
        )
    }

    fn rpc(method: &str, error: Value, json: bool) -> Self {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("JSON-RPC error");
        let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
        Self::new(
            format!("{method}: {message} (code {code})"),
            4,
            json,
            json!({ "kind": "rpc", "method": method, "rpc_error": error }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{Cli, Command};

    #[test]
    fn ls_defaults_to_root() {
        let cli = Cli::parse(["ls"].into_iter().map(String::from)).expect("parse");
        match cli.command {
            Command::Ls { path, json_output } => {
                assert_eq!(path, "/");
                assert!(!json_output);
            }
            Command::Cat { .. } => panic!("expected ls"),
            Command::State { .. } => panic!("expected ls"),
            Command::TaskCompleted { .. } | Command::TaskFailed { .. } => panic!("expected ls"),
        }
    }

    #[test]
    fn state_has_no_path() {
        let cli = Cli::parse(["state"].into_iter().map(String::from)).expect("parse");
        match cli.command {
            Command::State { json_output } => assert!(!json_output),
            Command::Ls { .. }
            | Command::Cat { .. }
            | Command::TaskCompleted { .. }
            | Command::TaskFailed { .. } => panic!("expected state"),
        }

        let err = Cli::parse(["state", "extra"].into_iter().map(String::from))
            .expect_err("state extra must not parse");
        assert!(
            err.message.contains("state takes no path argument"),
            "err = {err:?}"
        );
    }

    #[test]
    fn token_option_is_not_accepted() {
        let err = Cli::parse(["--token", "secret", "ls"].into_iter().map(String::from))
            .expect_err("token flag must not parse");
        assert!(err.message.contains("--token"), "err = {err:?}");
    }

    #[test]
    fn task_completed_parses_json_result_and_artifacts() {
        let cli = Cli::parse(
            [
                "task-completed",
                "--idempotency-key",
                "k1",
                "--result",
                r#"{"ok":true}"#,
                "--artifact",
                "out.log",
                "--json",
            ]
            .into_iter()
            .map(String::from),
        )
        .expect("parse");
        match cli.command {
            Command::TaskCompleted {
                idempotency_key,
                result,
                artifacts,
                json_errors,
            } => {
                assert_eq!(idempotency_key, "k1");
                assert_eq!(result.unwrap(), serde_json::json!({ "ok": true }));
                assert_eq!(artifacts, vec!["out.log"]);
                assert!(json_errors);
            }
            Command::Ls { .. } | Command::Cat { .. } | Command::State { .. } => {
                panic!("expected task-completed")
            }
            Command::TaskFailed { .. } => panic!("expected task-completed"),
        }
    }

    #[test]
    fn task_completed_parses_plain_text_result() {
        let cli = Cli::parse(
            [
                "task-completed",
                "--idempotency-key",
                "k1",
                "--result",
                "plain text",
            ]
            .into_iter()
            .map(String::from),
        )
        .expect("parse");
        match cli.command {
            Command::TaskCompleted {
                idempotency_key,
                result,
                artifacts,
                json_errors,
            } => {
                assert_eq!(idempotency_key, "k1");
                assert_eq!(result.unwrap(), serde_json::json!("plain text"));
                assert!(artifacts.is_empty());
                assert!(!json_errors);
            }
            Command::Ls { .. } | Command::Cat { .. } | Command::State { .. } => {
                panic!("expected task-completed")
            }
            Command::TaskFailed { .. } => panic!("expected task-completed"),
        }
    }

    #[test]
    fn task_failed_requires_reason() {
        let err = Cli::parse(
            ["task-failed", "--idempotency-key", "k1"]
                .into_iter()
                .map(String::from),
        )
        .expect_err("reason required");
        assert!(err.message.contains("--reason"), "err = {err:?}");
    }
}
