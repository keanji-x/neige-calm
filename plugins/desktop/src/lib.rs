//! The agent desktop plugin (#2530): a real Chrome on the headless
//! `compositor`, its main window streamed to Report `window` blocks through
//! `window-stream`, and its page driven through `chrome-control`.
//!
//! The process speaks only the plugin protocol: stdio MCP with the kernel
//! (the handshake, then `tools/call`) and HTTP on the manifest's
//! `http_socket`, which the kernel's session-gated WebSocket route dials. It
//! opens no TCP port, and Chrome talks CDP over a pipe.
//!
//! Lifecycle: `initialize` starts the compositor and binds `http.sock`;
//! Chrome starts on the first demand. Stdin EOF (the kernel is gone) and
//! SIGTERM stop Chrome (`Chrome::stop`) and end the process; a SIGKILL ends
//! Chrome through its `PR_SET_PDEATHSIG`.
#![cfg(target_os = "linux")]

pub mod app;
pub mod capture;
pub mod config;
pub mod feed;
pub mod keys;
pub mod layout;
pub mod mcp;
pub mod stream;
pub mod tools;
pub mod windows;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use compositor::Compositor;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::app::{ChromeApp, ChromeSetup};
use crate::config::{DesktopConfig, PluginEnv};
use crate::layout::Layout;
use crate::mcp::Inbound;
use crate::windows::Windows;

/// Everything a tool call or a viewer needs.
pub struct Desktop {
    pub env: PluginEnv,
    pub compositor: Compositor,
    pub windows: Windows,
    pub app: ChromeApp,
}

/// How long the exit path waits for the app task, which stops Chrome with a
/// 5 s grace before its group's SIGKILL.
const STOP_WAIT: Duration = Duration::from_secs(10);

/// The process entry point.
pub fn main() -> ExitCode {
    // stderr is a pipe to the kernel. When the kernel dies it breaks, and a
    // failed log write must not panic the way to Chrome's orderly stop.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .log_internal_errors(false)
        .with_ansi(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("DESKTOP_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let env = match PluginEnv::from_env() {
        Ok(env) => env,
        Err(e) => {
            eprintln!("desktop: {e}");
            return ExitCode::from(2);
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("desktop: tokio runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let code = runtime.block_on(run(env));
    // A blocking stdin read may still be parked (the SIGTERM path): do not
    // wait for it.
    runtime.shutdown_timeout(Duration::from_secs(1));
    code
}

/// A started desktop: what `initialize` set up.
struct Started {
    desktop: Arc<Desktop>,
    layout: Layout,
    app_task: tokio::task::JoinHandle<()>,
}

async fn run(env: PluginEnv) -> ExitCode {
    let (out, out_rx) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(write_frames(out_rx));
    let mut lines = Frames(BufReader::new(tokio::io::stdin()));
    let shutdown = CancellationToken::new();

    // Nothing runs before the handshake: it carries the configuration.
    let started = loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) | Err(_) => return ExitCode::SUCCESS,
        };
        let Ok(Inbound::Request { id, method, params }) = mcp::parse(&line) else {
            continue;
        };
        if method != "initialize" {
            let _ = out.send(mcp::error(
                &id,
                mcp::NOT_INITIALIZED,
                "not initialized",
                None,
            ));
            continue;
        }
        match start(&env, &params, &shutdown).await {
            Ok(started) => {
                let _ = out.send(mcp::ok(&id, mcp::initialize_result(&params, &env.token)));
                break started;
            }
            Err(e) => {
                tracing::error!("initialize failed: {e}");
                let _ = out.send(mcp::error(&id, tools::INTERNAL, &e, None));
                drop(out);
                let _ = writer.await;
                return ExitCode::FAILURE;
            }
        }
    };

    let mut terminate =
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(signal) => signal,
            Err(e) => {
                tracing::error!("cannot handle SIGTERM: {e}");
                return ExitCode::FAILURE;
            }
        };
    loop {
        tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(line)) => dispatch(&started.desktop, &line, &out),
                Ok(None) => { tracing::info!("stdin closed; stopping"); break }
                Err(e) => { tracing::info!("stdin failed ({e}); stopping"); break }
            },
            _ = terminate.recv() => { tracing::info!("SIGTERM; stopping"); break }
        }
    }
    shutdown.cancel();
    if tokio::time::timeout(STOP_WAIT, started.app_task)
        .await
        .is_err()
    {
        tracing::warn!("Chrome did not stop within {STOP_WAIT:?}");
    }
    let _ = std::fs::remove_dir_all(&started.layout.run);
    ExitCode::SUCCESS
}

/// Sets the desktop up for `initialize`: the private layout, the compositor,
/// `http.sock` and the app task. Chrome is not launched.
async fn start(
    env: &PluginEnv,
    params: &serde_json::Value,
    shutdown: &CancellationToken,
) -> Result<Started, String> {
    let config = DesktopConfig::from_values(mcp::config_values(params)?)?;
    let layout = Layout::new(&env.data_dir);
    layout
        .prepare()
        .map_err(|e| format!("prepare {}: {e}", env.data_dir.display()))?;
    let (compositor, events) = compositor::start(compositor::Config {
        run_dir: layout.run.clone(),
        size: config.size,
        max_fps: config.max_fps,
    })
    .map_err(|e| e.to_string())?;
    let windows = Windows::follow(events);
    let listener = layout::bind_http_socket(
        &layout.http_socket,
        layout::BIND_ATTEMPTS,
        layout::BIND_BACKOFF,
    )
    .await
    .map_err(|e| format!("bind {}: {e}", layout.http_socket.display()))?;
    let (app, app_task) = ChromeApp::spawn(
        ChromeSetup {
            binary: config.chrome_binary.clone(),
            profile_dir: layout.profile.clone(),
            home_dir: layout.home.clone(),
            wayland_socket: compositor.wayland_socket().to_path_buf(),
            size: config.size,
            proxy_server: config.proxy_server.clone(),
        },
        shutdown.clone(),
    );
    let desktop = Arc::new(Desktop {
        env: env.clone(),
        compositor,
        windows,
        app,
    });
    tokio::spawn(stream::serve(listener, desktop.clone(), shutdown.clone()));
    tracing::info!(
        id = %env.id,
        socket = %layout.http_socket.display(),
        size = ?config.size,
        max_fps = config.max_fps,
        "desktop started"
    );
    Ok(Started {
        desktop,
        layout,
        app_task,
    })
}

/// Answers one frame; tool calls run side by side.
fn dispatch(desktop: &Arc<Desktop>, line: &str, out: &mpsc::UnboundedSender<String>) {
    let (id, method, params) = match mcp::parse(line) {
        Ok(Inbound::Request { id, method, params }) => (id, method, params),
        Ok(Inbound::Other) => return,
        Err(e) => {
            tracing::warn!("ignoring a frame: {e}");
            return;
        }
    };
    let reply = match method.as_str() {
        "tools/call" => {
            let (tool, args) = match mcp::tool_call(&params) {
                Ok(call) => call,
                Err(e) => {
                    let _ = out.send(mcp::error(&id, tools::INVALID_PARAMS, &e, None));
                    return;
                }
            };
            let desktop = desktop.clone();
            let out = out.clone();
            tokio::spawn(async move {
                let frame = match tools::call(&desktop, &tool, args).await {
                    Ok(result) => mcp::ok(&id, result),
                    Err(e) => mcp::error(&id, e.code, &e.message, e.data),
                };
                let _ = out.send(frame);
            });
            return;
        }
        "ping" => mcp::ok(&id, serde_json::json!({})),
        other => mcp::error(
            &id,
            tools::UNKNOWN_TOOL,
            &format!("method `{other}` is not served"),
            None,
        ),
    };
    let _ = out.send(reply);
}

/// Newline-delimited frames from the kernel. A line that is not UTF-8 is a
/// malformed frame, skipped like any other; only EOF or a read error ends it.
struct Frames<R>(R);

impl<R: AsyncBufRead + Unpin> Frames<R> {
    async fn next_line(&mut self) -> std::io::Result<Option<String>> {
        let mut line = Vec::new();
        loop {
            line.clear();
            if self.0.read_until(b'\n', &mut line).await? == 0 {
                return Ok(None);
            }
            match String::from_utf8(std::mem::take(&mut line)) {
                Ok(text) => return Ok(Some(text.trim_end_matches(['\n', '\r']).to_owned())),
                Err(_) => tracing::warn!("ignoring a frame that is not UTF-8"),
            }
        }
    }
}

async fn write_frames(mut frames: mpsc::UnboundedReceiver<String>) {
    let mut stdout = tokio::io::stdout();
    while let Some(mut frame) = frames.recv().await {
        frame.push('\n');
        if stdout.write_all(frame.as_bytes()).await.is_err() || stdout.flush().await.is_err() {
            return;
        }
    }
}
