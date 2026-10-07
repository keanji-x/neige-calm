mod proc_probe;

// pin_lost_on_autoreap changes SIGCHLD for the entire process; keep it separate.
#[path = "attach_race_no_byte_loss.rs"]
mod attach_race_no_byte_loss;
#[path = "exit_status_survives_the_wnowait_split.rs"]
mod exit_status_survives_the_wnowait_split;
#[path = "explicit_child_environment.rs"]
mod explicit_child_environment;
#[path = "no_wildcard_wait_in_the_supervisor_host.rs"]
mod no_wildcard_wait_in_the_supervisor_host;
#[path = "pipe_procs_are_not_signalable.rs"]
mod pipe_procs_are_not_signalable;
#[path = "pty_cwd.rs"]
mod pty_cwd;
#[path = "pty_entry_reclaim.rs"]
mod pty_entry_reclaim;
#[path = "pty_exit_ordering.rs"]
mod pty_exit_ordering;
#[path = "pty_pid_pinning.rs"]
mod pty_pid_pinning;
#[path = "pty_proc_byte_stream_and_replay.rs"]
mod pty_proc_byte_stream_and_replay;
#[path = "pty_writestdin_acked.rs"]
mod pty_writestdin_acked;
#[path = "server_restart_survives.rs"]
mod server_restart_survives;
#[path = "signal_terminates_pty_child.rs"]
mod signal_terminates_pty_child;
#[path = "terminate_all_after_exit_recorded.rs"]
mod terminate_all_after_exit_recorded;
#[path = "terminate_all_kills_grandchild.rs"]
mod terminate_all_kills_grandchild;
#[path = "terminate_all_kills_grandchild_in_drain_grace.rs"]
mod terminate_all_kills_grandchild_in_drain_grace;

#[test]
fn every_root_test_file_is_in_this_suite() {
    let mut declared: Vec<_> = include_str!("integration_suite.rs")
        .lines()
        .filter_map(|line| line.trim().strip_prefix("#[path = \"")?.strip_suffix("\"]"))
        .map(str::to_owned)
        .collect();
    declared.push("integration_suite.rs".to_owned());
    declared.push("pin_lost_on_autoreap.rs".to_owned());
    let manifest = include_str!("../Cargo.toml");
    for name in ["integration_suite", "pin_lost_on_autoreap"] {
        let registration = format!("[[test]]\nname = \"{name}\"\npath = \"tests/{name}.rs\"");
        assert!(
            manifest.contains(&registration),
            "missing test target: {name}"
        );
    }
    let mut actual: Vec<_> = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/tests"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".rs"))
        .collect();
    declared.sort_unstable();
    actual.sort_unstable();
    assert_eq!(
        declared, actual,
        "a root test file is missing or registered more than once"
    );
}
