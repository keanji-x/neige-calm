//! Exercise the native readiness entry point, including caller cancellation.
use super::readiness_command::{RunFailure, run};
use std::{path::Path, time::Duration};

fn alive(pid: i32) -> bool {
    crate::proc_identity::read_proc_start_time(pid).is_some()
        && std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .is_ok_and(|stat| !stat.split_once(") ").unwrap().1.starts_with('Z'))
}

#[tokio::test]
async fn c4_claude_readiness_kills_group_on_timeout_and_cancel() {
    let mut leaked = Vec::new();
    for mode in ["success", "timeout", "cancel"] {
        let root = tempfile::tempdir().unwrap();
        let pid_file = root.path().join("pid");
        let release = root.path().join("release");
        let script = format!(
            "sleep 300 </dev/null >/dev/null 2>&1 & echo $! > '{}'; while [ ! -f '{}' ]; do sleep 0.01; done; {}",
            pid_file.display(),
            release.display(),
            if mode == "success" { "exit 0" } else { "wait" },
        );
        let task = tokio::spawn(async move {
            run(
                Path::new("/bin/sh"),
                &["-c", &script],
                &[],
                Duration::from_millis(500),
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(3), async {
            while !pid_file.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let pid: i32 = std::fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let identity = crate::proc_identity::read_proc_start_time(pid).unwrap();
        std::fs::write(release, "").unwrap();
        if mode == "cancel" {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        } else {
            let result = task.await.unwrap();
            if mode == "success" {
                assert!(result.unwrap().0.success());
            } else {
                assert!(matches!(result, Err(RunFailure::TimedOut(_))));
            }
        }
        let stopped = tokio::time::timeout(Duration::from_secs(2), async {
            while alive(pid) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        // Reproduction cleanup also runs when the group guard is absent.
        if crate::proc_identity::read_proc_start_time(pid) == Some(identity) {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
        if stopped.is_err() {
            leaked.push(mode);
        }
    }
    assert!(
        leaked.is_empty(),
        "readiness descendants survived: {leaked:?}"
    );
}
