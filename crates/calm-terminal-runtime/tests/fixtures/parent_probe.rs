//! Exercises the production launch builder under a deliberately tainted parent.
use calm_terminal_runtime::RuntimeLaunch;
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args_os().collect();
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
