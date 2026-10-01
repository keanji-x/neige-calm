//! Stop evidence for an owned execution, including descendants that changed process group.
use crate::{Error, Result, proc_entry_vanished};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const MARKER_KEY: &str = "NEIGE_EXECUTION_OP";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessArtifacts {
    pub pid: i32,
    pub pgid: i32,
    pub start_time: u64,
    pub boot_id: String,
}

struct Member {
    pid: i32,
    group: i32,
    start: u64,
    live: bool,
}
fn member(path: &Path, pid: i32) -> Result<Member> {
    let text = std::fs::read_to_string(path.join("stat"))?;
    let tail = text
        .rsplit_once(')')
        .ok_or_else(|| Error::Evidence("invalid process stat".into()))?
        .1;
    let fields: Vec<_> = tail.split_whitespace().collect();
    let group = fields
        .get(2)
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| Error::Evidence("missing process group".into()))?;
    let start = fields
        .get(19)
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| Error::Evidence("missing process start time".into()))?;
    Ok(Member {
        pid,
        group,
        start,
        live: !matches!(fields.first(), Some(&"Z" | &"X")),
    })
}
pub fn capture(pid: i32) -> Result<ProcessArtifacts> {
    let m = member(&Path::new("/proc").join(pid.to_string()), pid)?;
    Ok(ProcessArtifacts {
        pid,
        pgid: m.group,
        start_time: m.start,
        boot_id: std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?
            .trim()
            .into(),
    })
}

/// A pass authenticates each signal by marker and start time; a dead leader alone is never proof.
/// Call repeatedly under a deadline. An unreadable live member of the original group blocks stop.
/// The marker is a contract of these trusted providers; it must survive descendant execution.
pub fn stop_pass(artifacts: &ProcessArtifacts, marker: &str) -> Result<bool> {
    if artifacts.pgid <= 1 {
        return Err(Error::Evidence("invalid execution identity".into()));
    }
    if std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?.trim() != artifacts.boot_id {
        return Ok(true);
    }
    scan_stop(Some(artifacts.pgid), marker)
}

/// Recovery without a surviving leader still sweeps the exact durable execution marker.
pub fn stop_marker_pass(marker: &str) -> Result<bool> {
    scan_stop(None, marker)
}
fn scan_stop(group: Option<i32>, marker: &str) -> Result<bool> {
    if marker.is_empty() {
        return Err(Error::Evidence("missing execution marker".into()));
    }
    let needle = format!("{MARKER_KEY}={marker}");
    let mut stopped = true;
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        let m = match member(&entry.path(), pid) {
            Ok(m) => m,
            Err(Error::Io(e)) if proc_entry_vanished(&e) => continue,
            Err(e) => return Err(e),
        };
        if !m.live {
            continue;
        }
        let env = match std::fs::read(entry.path().join("environ")) {
            Ok(env) if !env.is_empty() => env,
            Err(e) if proc_entry_vanished(&e) => continue,
            _ => {
                if Some(m.group) == group {
                    stopped = false;
                }
                continue;
            }
        };
        if !env.split(|b| *b == 0).any(|e| e == needle.as_bytes()) {
            continue;
        }
        stopped = false;
        if pid <= 1 || pid == std::process::id() as i32 {
            continue;
        }
        // Re-read identity and authentication before signaling a positive PID, never a reused PGID.
        let Ok(current) = member(&entry.path(), pid) else {
            continue;
        };
        let Ok(env) = std::fs::read(entry.path().join("environ")) else {
            continue;
        };
        if current.live
            && current.start == m.start
            && env.split(|b| *b == 0).any(|e| e == needle.as_bytes())
        {
            // SAFETY: positive PID authenticated by its inherited marker and verified start time.
            unsafe {
                libc::kill(m.pid, libc::SIGKILL);
            }
        }
    }
    Ok(stopped)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    #[test]
    fn execution_stop_reaches_descendant_in_another_group_without_killing_foreign_process() {
        let marker = uuid::Uuid::new_v4().to_string();
        let mut child = Command::new("/usr/bin/setsid")
            .args(["/bin/sh", "-c", "setsid sleep 60 & wait"])
            .env(MARKER_KEY, &marker)
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let mut foreign = Command::new("sleep").arg("60").spawn().unwrap();
        std::thread::sleep(Duration::from_millis(80));
        let artifacts = capture(child.id() as i32).unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        while !stop_pass(&artifacts, &marker).unwrap() {
            assert!(Instant::now() < until, "owned descendants remain alive");
            std::thread::sleep(Duration::from_millis(20));
        }
        child.wait().unwrap();
        assert!(foreign.try_wait().unwrap().is_none());
        foreign.kill().unwrap();
        foreign.wait().unwrap();
    }
}
