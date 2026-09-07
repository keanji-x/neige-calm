//! Exercises the production launch builder under a deliberately tainted parent.
use calm_terminal_runtime::RuntimeLaunch;
use std::path::PathBuf;

#[tokio::main(worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args_os().collect();
    #[cfg(unix)]
    if args.get(1).is_some_and(|arg| arg == "--descendant-client") {
        use std::io::Read;
        // The parent starts this through a shell with HUP/TERM ignored.
        // Keep this connection open until killed or the test releases us.
        let mut socket = std::os::unix::net::UnixStream::connect(&args[2])?;
        let _ = socket.read(&mut [0u8; 1]);
        return Ok(());
    }
    if args.get(1).is_some_and(|arg| arg == "--pane-probe") {
        return pane_probe(PathBuf::from(&args[2]), PathBuf::from(&args[3])).await;
    }
    if args.get(1).is_some_and(|arg| arg == "--socket") {
        anyhow::ensure!(
            std::env::var_os("NEIGE_TEST_PARENT_SENTINEL").is_none(),
            "parent environment leaked"
        );
        anyhow::ensure!(
            std::env::var_os("RMUX_CONFIG_FILE").is_none(),
            "implicit rmux configuration leaked"
        );
        println!("clean child environment");
        return Ok(());
    }
    let root = PathBuf::from(args.get(1).expect("private root argument"));
    let launch = RuntimeLaunch {
        executable: std::env::current_exe()?,
        socket: root.join("runtime.sock"),
        cwd: root.clone(),
        home: root,
        executable_path: "/usr/bin:/bin".into(),
        locale: "C.UTF-8".into(),
    };
    let output = launch.command()?.output()?;
    anyhow::ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    anyhow::ensure!(
        output.stdout == b"clean child environment\n",
        "unexpected probe output"
    );
    Ok(())
}

struct OwnedChild(std::process::Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

async fn pane_probe(root: PathBuf, executable: PathBuf) -> anyhow::Result<()> {
    use std::time::Duration;
    let launch = RuntimeLaunch {
        executable,
        socket: root.join("runtime.sock"),
        cwd: root.clone(),
        home: root,
        executable_path: "/usr/bin:/bin".into(),
        locale: "C.UTF-8".into(),
    };
    let mut child = OwnedChild(
        launch
            .command()?
            .stdout(std::process::Stdio::null())
            .spawn()?,
    );
    let client = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            anyhow::ensure!(
                child.0.try_wait()?.is_none(),
                "runtime exited during startup"
            );
            if let Ok(client) =
                calm_terminal_runtime::connect(&launch.socket, Duration::from_secs(1)).await
            {
                return Ok::<_, anyhow::Error>(client);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??;
    let result = async {
        let pane = client
            .create(calm_terminal_runtime::TerminalLaunchConfig {
                name: "environment-probe".into(),
                cwd: launch.cwd.clone(),
                cols: 80,
                rows: 24,
                argv: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "printf 'sentinel:%s\\n' \"${NEIGE_TEST_PARENT_SENTINEL-absent}\"; read line"
                        .into(),
                ],
                environment: std::collections::BTreeMap::new(),
            })
            .await?;
        pane.wait_for_text("sentinel:").await?;
        anyhow::ensure!(
            pane.snapshot()
                .await?
                .visible_lines()
                .join("\n")
                .contains("sentinel:absent"),
            "calling application's environment leaked into the actual shell"
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let shutdown = client.shutdown().await;
    result?;
    shutdown?;
    Ok(())
}
