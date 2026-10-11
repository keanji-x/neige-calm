//! A policy run by the real `/usr/bin/bwrap`: what the program inside sees.
#![cfg(target_os = "linux")]

use std::collections::BTreeSet;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use sandbox::{Etc, Policy};

const BWRAP: &str = "/usr/bin/bwrap";

/// Runs `script` with `/bin/sh` inside `policy`; its stdout lines.
fn run(policy: &Policy, script: &str) -> Vec<String> {
    let out = Command::new(policy.program())
        .args(policy.args())
        .args(["/bin/sh", "-c", script])
        .stdin(Stdio::null())
        .output()
        .expect("run bwrap");
    assert!(
        out.status.success(),
        "{}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_program_sees_its_binds_the_etc_allowlist_and_nothing_else_of_the_host() {
    let host = tempfile::tempdir().unwrap();
    let (ro, rw, hidden) = (
        host.path().join("ro"),
        host.path().join("rw"),
        host.path().join("hidden"),
    );
    for dir in [&ro, &rw, &hidden] {
        std::fs::create_dir(dir).unwrap();
    }
    std::fs::write(ro.join("given"), b"read me").unwrap();
    std::fs::write(hidden.join("secret"), b"never").unwrap();
    let mut policy = Policy::new(BWRAP)
        .unwrap()
        .read_only(&ro, "/data/ro")
        .unwrap()
        .read_write(&rw, "/data/rw")
        .unwrap()
        .env("SANDBOX_TEST", "inside")
        .unwrap();
    for entry in Etc::ALL {
        policy = policy.etc(entry);
    }
    let hidden_path = hidden.join("secret");
    let script = format!(
        "cat /data/ro/given; echo; \
         (touch /data/ro/new 2>/dev/null && echo ro-writable) || echo ro-refused; \
         echo written > /data/rw/out && echo rw-ok; \
         (test -e '{hidden}' && echo hidden-visible) || echo hidden-absent; \
         (test -e '{host}' && echo host-tmp-visible) || echo host-tmp-absent; \
         echo \"env=$SANDBOX_TEST\"; \
         ls -A /etc | sed 's/^/etc:/'; \
         ls -d /proc/[0-9]* | wc -l | sed 's/^/procs:/'; \
         readlink /proc/self/ns/net | sed 's/^/net:/'",
        hidden = hidden_path.display(),
        host = host.path().display(),
    );
    let lines = run(&policy, &script);
    let take = |prefix: &str| -> Vec<String> {
        lines
            .iter()
            .filter_map(|l| l.strip_prefix(prefix).map(str::to_owned))
            .collect()
    };
    assert_eq!(
        &lines[..6],
        [
            "read me",
            "ro-refused",
            "rw-ok",
            "hidden-absent",
            "host-tmp-absent",
            "env=inside"
        ],
        "{lines:?}"
    );
    assert_eq!(
        std::fs::read_to_string(rw.join("out")).unwrap(),
        "written\n"
    );
    assert!(!ro.join("new").exists());

    // Only allowlisted /etc entries, and only those the host has.
    let allowed: BTreeSet<String> = Etc::ALL
        .iter()
        .map(|e| {
            let rest = e.path().strip_prefix("/etc/").unwrap();
            rest.split('/').next().unwrap().to_owned()
        })
        .collect();
    let etc: BTreeSet<String> = take("etc:").into_iter().collect();
    assert!(etc.is_subset(&allowed), "{etc:?} not within {allowed:?}");
    assert!(etc.contains("hosts"), "{etc:?}");

    // Its own pid namespace (bwrap's init, the shell and its pipeline), the
    // host's network.
    let procs: usize = take("procs:")[0].trim().parse().unwrap();
    assert!(procs <= 6, "{procs} processes visible");
    let host_net = std::fs::read_link("/proc/self/ns/net").unwrap();
    assert_eq!(take("net:"), [host_net.to_string_lossy().into_owned()]);
}

/// Killing bwrap ends everything inside: the program is in the sandbox's pid
/// namespace, which dies with its init.
#[test]
fn killing_bwrap_ends_the_program_inside() {
    let policy = Policy::new(BWRAP).unwrap();
    let mut bwrap = Command::new(policy.program())
        .args(policy.args())
        .args(["/bin/sh", "-c", "sleep 60 & sleep 60"])
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let inside = descendants_within(bwrap.id(), 4, Duration::from_secs(10));
    bwrap.kill().unwrap();
    bwrap.wait().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while inside.iter().any(|p| p.alive()) {
        assert!(Instant::now() < deadline, "survivors: {inside:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A process identity that survives pid reuse.
#[derive(Debug)]
struct Proc {
    pid: u32,
    start: u64,
}

impl Proc {
    fn alive(&self) -> bool {
        stat(self.pid).is_some_and(|(state, _, start)| start == self.start && state != 'Z')
    }
}

/// (state, ppid, start time) from `/proc/<pid>/stat`.
fn stat(pid: u32) -> Option<(char, u32, u64)> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields: Vec<&str> = text.rsplit_once(')')?.1.split_whitespace().collect();
    Some((
        fields[0].chars().next()?,
        fields[1].parse().ok()?,
        fields[19].parse().ok()?,
    ))
}

/// Waits until `root` has at least `count` descendants; returns them.
fn descendants_within(root: u32, count: usize, within: Duration) -> Vec<Proc> {
    let deadline = Instant::now() + within;
    loop {
        let all: Vec<(u32, u32, u64)> = std::fs::read_dir("/proc")
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
            .filter_map(|pid| stat(pid).map(|(_, ppid, start)| (pid, ppid, start)))
            .collect();
        let mut found: Vec<Proc> = Vec::new();
        let mut frontier = vec![root];
        while let Some(parent) = frontier.pop() {
            for &(pid, ppid, start) in &all {
                if ppid == parent {
                    found.push(Proc { pid, start });
                    frontier.push(pid);
                }
            }
        }
        if found.len() >= count {
            return found;
        }
        assert!(Instant::now() < deadline, "only {found:?} under {root}");
        std::thread::sleep(Duration::from_millis(20));
    }
}
