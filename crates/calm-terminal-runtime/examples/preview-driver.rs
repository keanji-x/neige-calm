//! Developer-only real-TUI driver. NDJSON over stdin/stdout; not an MCP server
//! or an application authorization boundary. No user/model configuration loads.
use std::collections::BTreeMap;
use std::io::{BufRead, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::time::Duration;

use calm_terminal_runtime::{RuntimeLaunch, TerminalLaunchConfig, connect};
use clap::Parser;
use serde_json::{Value, json};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    runtime_executable: PathBuf,
    #[arg(long)]
    cwd: PathBuf,
    #[arg(long)]
    program: PathBuf,
    #[arg(last = true)]
    argv: Vec<String>,
}

struct OwnedHost(Child);
impl Drop for OwnedHost {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

#[tokio::main(worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    anyhow::ensure!(args.program.is_absolute(), "absolute program required");
    let root = tempfile::tempdir()?;
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))?;
    let launch = RuntimeLaunch {
        executable: args.runtime_executable,
        socket: root.path().join("runtime.sock"),
        cwd: args.cwd.clone(),
        home: root.path().to_owned(),
        executable_path: "/usr/bin:/bin".into(),
        locale: "C.UTF-8".into(),
    };
    let mut host = OwnedHost(
        launch
            .isolated_command("/usr/bin/unshare".as_ref())?
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()?,
    );
    let budget = Duration::from_secs(5);
    let client = tokio::time::timeout(budget, async {
        loop {
            anyhow::ensure!(host.0.try_wait()?.is_none(), "runtime exited during launch");
            if let Ok(client) = connect(&launch.socket, budget).await {
                return Ok::<_, anyhow::Error>(client);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??;
    let mut argv = vec![
        args.program
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("UTF-8 program required"))?
            .to_owned(),
    ];
    argv.extend(args.argv);
    let pane = client
        .create(TerminalLaunchConfig {
            name: "preview".into(),
            argv,
            cwd: args.cwd,
            cols: 80,
            rows: 24,
            environment: BTreeMap::from([
                ("HOME".into(), root.path().to_str().unwrap().into()),
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("LANG".into(), "C.UTF-8".into()),
            ]),
        })
        .await?;
    println!("{}", json!({"ready":true}));
    std::io::stdout().flush()?;
    // This executable has no UI/reactor input of its own. Only its trusted
    // developer harness supplies lines; do not expose this pipe to an app user.
    let (sender, mut incoming) = tokio::sync::mpsc::channel(1);
    // A native reader thread allows the async control loop to process parent
    // cancellation while stdin remains open. Process exit owns this thread.
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut input = stdin.lock();
        loop {
            let mut line = String::new();
            let result = match (&mut input).take(65537).read_line(&mut line) {
                Ok(0) => break,
                Ok(_) if line.len() <= 65536 => Ok(line),
                Ok(_) => Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "preview request too large",
                )),
                Err(error) => Err(error),
            };
            let failed = result.is_err();
            if sender.blocking_send(result).is_err() || failed {
                break;
            }
        }
    });
    let interaction = async {
        while let Some(line) = incoming.recv().await {
            let line = line?;
            let request: Value = serde_json::from_str(&line)?;
            let result: anyhow::Result<Value> = async {
            match request["action"].as_str() {
                Some("observe") => {
                    let capture = pane.observe().await?;
                    Ok(json!({"keyframe": capture.keyframe, "snapshot": capture.snapshot,
                        "generation":capture.generation.to_string(), "next_sequence":capture.next_sequence.to_string(),
                        "alternate":capture.alternate, "history_rows_total":capture.coverage.history_rows_total,
                        "history_rows_included":capture.coverage.history_rows_included,
                        "metadata_complete":capture.coverage.metadata_complete}))
                }
                Some("text") => { pane.send_text(request["text"].as_str().ok_or_else(|| anyhow::anyhow!("text required"))?).await?; Ok(json!({"sent":true})) }
                Some("key") => { pane.send_key(request["key"].as_str().ok_or_else(|| anyhow::anyhow!("key required"))?).await?; Ok(json!({"sent":true})) }
                _ => anyhow::bail!("unknown preview action"),
            }
        }.await;
            println!(
                "{}",
                match result {
                    Ok(value) => json!({"result":value}),
                    Err(error) => json!({"error":error.to_string()}),
                }
            );
            std::io::stdout().flush()?;
        }
        Ok::<_, anyhow::Error>(())
    };
    let interaction = tokio::select! {
        result = interaction => result,
        _ = terminate.recv() => Err(anyhow::anyhow!("preview parent terminated")),
        _ = interrupt.recv() => Err(anyhow::anyhow!("preview parent interrupted")),
    };
    // Cancellation and malformed input still pass through owned host cleanup.
    tokio::time::timeout(budget, client.shutdown()).await??;

    let status = tokio::time::timeout(budget, async {
        loop {
            if let Some(status) = host.0.try_wait()? {
                return Ok::<_, std::io::Error>(status);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??;
    anyhow::ensure!(status.success(), "runtime shutdown failed");
    interaction
}
