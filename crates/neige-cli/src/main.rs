//! `neige`: forwards argv to the kernel, which parses, runs and renders every command, and writes the
//! kernel's `{stdout, stderr, exit}` back verbatim. Frozen forwarding protocol v1:
//! docs/architecture/1801-kernel-served-cli.md §3. Only a lone `--version` and the §3.3 failures are local.

use std::env;
use std::ffi::OsString;
use std::io::{self, Write};
use std::process::ExitCode;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

const ENV_SOCKET: &str = "NEIGE_MCP_SOCKET";
const ENV_TOKEN: &str = "NEIGE_MCP_TOKEN";
/// 128 + SIGPIPE: stdout or stderr is gone, so nothing more can be said.
const EXIT_WRITE_FAILED: u8 = 141;

/// A local failure: its stderr line and exit code.
struct Failure(String, u8);

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args: Vec<OsString> = env::args_os().skip(1).collect();
    if args.len() == 1 && args[0] == "--version" {
        return emit(
            format!("neige {}\n", env!("CARGO_PKG_VERSION")).as_bytes(),
            b"",
            0,
        );
    }
    match forward(args).await {
        Ok((stdout, stderr, exit)) => emit(stdout.as_bytes(), stderr.as_bytes(), exit),
        Err(Failure(message, exit)) => emit(b"", format!("neige: {message}\n").as_bytes(), exit),
    }
}

fn emit(stdout: &[u8], stderr: &[u8], exit: u8) -> ExitCode {
    let written = io::stdout()
        .write_all(stdout)
        .and_then(|()| io::stdout().flush())
        .and_then(|()| io::stderr().write_all(stderr));
    ExitCode::from(if written.is_ok() {
        exit
    } else {
        EXIT_WRITE_FAILED
    })
}

fn env_var(name: &str) -> Result<OsString, Failure> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            Failure(
                format!("missing {name} env var; run from a neige planner terminal"),
                2,
            )
        })
}

async fn forward(args: Vec<OsString>) -> Result<(String, String, u8), Failure> {
    let socket = env_var(ENV_SOCKET)?;
    let token = env_var(ENV_TOKEN)?;
    let argv = args
        .into_iter()
        .enumerate()
        .map(|(index, arg)| {
            arg.into_string()
                .map_err(|_| Failure(format!("argument {} is not valid UTF-8", index + 1), 5))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let stream = UnixStream::connect(&socket)
        .await
        .map_err(|e| Failure(format!("connect {}: {e}", socket.to_string_lossy()), 3))?;
    let (rd, mut wr) = stream.into_split();
    let mut reader = BufReader::new(rd);
    // A token that is not UTF-8 cannot be a kernel token; the kernel refuses it (-32401).
    let token = serde_json::to_string(&token.to_string_lossy()).expect("a string serializes");
    let initialize = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2024-11-05","capabilities":{{}},"clientInfo":{{"name":"neige-forward","version":"1"}},"_meta":{{"dev.neige/auth":{{"token":{token}}}}}}}}}"#
    );
    request(&mut reader, &mut wr, "initialize", &initialize).await?;
    let argv = serde_json::to_string(&argv).expect("strings serialize");
    let cli =
        format!(r#"{{"jsonrpc":"2.0","id":2,"method":"neige/cli","params":{{"argv":{argv}}}}}"#);
    let result = request(&mut reader, &mut wr, "neige/cli", &cli).await?;
    let text = |key: &str| result.get(key).and_then(Value::as_str).map(str::to_owned);
    let exit = result
        .get("exit")
        .and_then(Value::as_u64)
        .and_then(|exit| u8::try_from(exit).ok());
    match (text("stdout"), text("stderr"), exit) {
        (Some(stdout), Some(stderr), Some(exit)) => Ok((stdout, stderr, exit)),
        _ => Err(Failure(format!("neige/cli: invalid result: {result}"), 4)),
    }
}

/// Write one frame, read one response line, return its `result`.
async fn request(
    reader: &mut BufReader<OwnedReadHalf>,
    wr: &mut OwnedWriteHalf,
    method: &str,
    line: &str,
) -> Result<Value, Failure> {
    let failed = |message: String| Failure(format!("{method}: {message}"), 4);
    let mut frame = line.as_bytes().to_vec();
    frame.push(b'\n');
    wr.write_all(&frame)
        .await
        .map_err(|e| failed(format!("write: {e}")))?;
    let mut response = String::new();
    let read = reader
        .read_line(&mut response)
        .await
        .map_err(|e| failed(format!("read: {e}")))?;
    if read == 0 {
        return Err(failed("server closed connection".into()));
    }
    let mut response: Value =
        serde_json::from_str(&response).map_err(|e| failed(format!("invalid response: {e}")))?;
    if let Some(error) = response.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("JSON-RPC error");
        return Err(failed(match error.get("code").and_then(Value::as_i64) {
            Some(code) => format!("{message} (code {code})"),
            None => message.to_string(),
        }));
    }
    response
        .get_mut("result")
        .map(Value::take)
        .ok_or_else(|| failed("response has neither result nor error".into()))
}
