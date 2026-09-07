//! Developer-only adapter: actual authenticated MCP tools and actual PTY.
//! No Planner/model is started. Explicit user commands arrive on private stdin.
#[allow(dead_code)]
#[path = "../tests/support/terminal_interaction.rs"]
mod support;
use serde_json::{Value, json};
use std::io::{BufRead, Read, Write};

#[tokio::main(worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    let harness = support::Harness::start().await;
    let (sender, mut incoming) = tokio::sync::mpsc::channel(1);
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut input = stdin.lock();
        loop {
            let mut line = String::new();
            let result = match (&mut input).take(65537).read_line(&mut line) {
                Ok(0) => break,
                Ok(_) if line.len() <= 65536 => Ok(line),
                Ok(_) => Err(std::io::Error::other("request too large")),
                Err(error) => Err(error),
            };
            if sender.blocking_send(result).is_err() {
                break;
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    // Private loopback preview exposes only this disposable test application's WS.
    let app = calm_server::ws::terminal::router().with_state(harness.state.clone());
    let browser = tokio::spawn(async move { axum::serve(listener, app).await });
    println!(
        "{}",
        json!({"ready":true,"http":format!("http://{address}"),"workspace":harness.root.path()})
    );
    std::io::stdout().flush()?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut terminals = Vec::<String>::new();
    let run = async {
        while let Some(line) = incoming.recv().await {
            let request: Value = serde_json::from_str(&line?)?;
            let name = request["name"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("tool name required"))?;
            anyhow::ensure!(
                name.starts_with("calm.terminal."),
                "preview accepts terminal tools only"
            );
            let result = harness.call(name, request["arguments"].clone()).await;
            if name == "calm.terminal.open"
                && let Some(terminal) =
                    result["result"]["structuredContent"]["terminal_id"].as_str()
                && !terminals.iter().any(|value| value == terminal)
            {
                terminals.push(terminal.to_owned());
            }
            println!("{result}");
            std::io::stdout().flush()?;
        }
        Ok::<_, anyhow::Error>(())
    };
    let result = tokio::select! {result=run=>result,_=terminate.recv()=>Ok(())};
    browser.abort();
    let _ = browser.await;
    for terminal in terminals {
        harness.state.terminal_renderer.drop_entry(&terminal).await;
    }
    result
}
