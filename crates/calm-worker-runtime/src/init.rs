use crate::{Error, LaunchConfig, NetworkPolicy, Result};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::os::fd::FromRawFd;
use std::process::{Command, Stdio};

#[derive(Serialize, Deserialize)]
pub(crate) struct StartMessage {
    pub token: String,
    pub launch_config: LaunchConfig,
}

pub(crate) fn check() -> Result<()> {
    if std::process::id() != 1 {
        return Err(Error::Unsupported("helper must be namespace PID 1".into()));
    }
    if std::fs::read_link("/proc/self")? != std::path::Path::new("1") {
        return Err(Error::Unsupported("private proc mount required".into()));
    }
    if unsafe {
        libc::syscall(
            libc::SYS_close_range,
            1024u32,
            u32::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        )
    } < 0
    {
        return Err(Error::Unsupported(format!(
            "close_range: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}

pub(crate) fn check_network(policy: NetworkPolicy, parent_inode: u64) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    check()?;
    let child_inode = std::fs::metadata("/proc/self/ns/net")?.ino();
    if (child_inode == parent_inode) != (policy == NetworkPolicy::Provider) {
        return Err(Error::Unsupported(
            "network namespace does not match requested policy".into(),
        ));
    }
    Ok(())
}

pub(crate) fn run(expected_token: &str) -> Result<()> {
    check()?;
    // This read end is installed by our launcher. Provider receives neither end.
    let mut gate = unsafe { std::fs::File::from_raw_fd(3) };
    let message = read_start(&mut gate, expected_token)?;
    drop(gate);
    launch(message)
}

fn read_start(gate: &mut impl Read, expected_token: &str) -> Result<StartMessage> {
    let mut length = [0u8; 4];
    gate.read_exact(&mut length)?; // EOF never authorizes execution.
    let length = u32::from_be_bytes(length) as usize;
    if length > 1_048_576 {
        return Err(Error::Conflict("oversized start message".into()));
    }
    let mut bytes = vec![0; length];
    gate.read_exact(&mut bytes)?;
    let message: StartMessage = serde_json::from_slice(&bytes)?;
    if message.token != expected_token {
        return Err(Error::Conflict("positive start token mismatch".into()));
    }
    Ok(message)
}

fn launch(message: StartMessage) -> Result<()> {
    // FD 5 is provider-only stdout. bwrap's monitor has no copy of this pipe.
    if unsafe { libc::dup2(5, 1) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // No ambient FDs from the host may be delegated to provider code.
    if unsafe { libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut command = Command::new(&message.launch_config.program);
    command
        .args(&message.launch_config.args)
        .env_clear()
        .envs(&message.launch_config.environment)
        .current_dir("/workspace")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let child = command.spawn()?;
    let provider_pid = child.id() as i32;
    // The init reaper must not retain provider stdin/stdout pipe ends. Otherwise
    // a live provider closing stdout could never produce observable stream EOF.
    use std::os::fd::AsRawFd;
    let null = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")?;
    if unsafe { libc::dup2(null.as_raw_fd(), 0) } < 0
        || unsafe { libc::dup2(null.as_raw_fd(), 1) } < 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    drop(null);
    // The provider is one specific child. The namespace init also owns adopted
    // direct children, inventoried below; neither path consumes arbitrary waits.
    drop(child);
    loop {
        if let Some(code) = poll_owned_child(provider_pid)? {
            std::process::exit(code);
        }
        reap_adopted_children(provider_pid)?;
        // No blocking provider wait: adopted zombies are collected while it is
        // still active. One private-proc inventory per tick, at most 50Hz idle.
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn reap_adopted_children(provider_pid: i32) -> Result<()> {
    if std::process::id() != 1 {
        return Err(Error::Evidence(
            "child inventory requires namespace init".into(),
        ));
    }
    let children = std::fs::read_to_string("/proc/self/task/1/children")?;
    for value in children.split_whitespace() {
        let pid = value
            .parse::<i32>()
            .map_err(|_| Error::Evidence("invalid direct child PID".into()))?;
        if pid == provider_pid {
            continue;
        }
        match poll_owned_child(pid) {
            Ok(_) => {}
            Err(Error::Io(error)) if error.raw_os_error() == Some(libc::ECHILD) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// A positive, caller-owned child only; WNOHANG cannot stall orphan collection.
fn poll_owned_child(pid: i32) -> Result<Option<i32>> {
    if pid <= 1 {
        return Err(Error::Evidence("specific child PID required".into()));
    }
    let mut status = 0;
    let result = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
    if result == 0 {
        return Ok(None);
    }
    if result < 0 {
        let error = std::io::Error::last_os_error();
        return if error.kind() == std::io::ErrorKind::Interrupted {
            Ok(None)
        } else {
            Err(error.into())
        };
    }
    if libc::WIFEXITED(status) {
        return Ok(Some(libc::WEXITSTATUS(status)));
    }
    if libc::WIFSIGNALED(status) {
        return Ok(Some(128 + libc::WTERMSIG(status)));
    }
    Err(Error::Evidence(
        "child wait returned nonterminal status".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn boundary_specific_child_wait_preserves_exit_codes_and_sibling_ownership() {
        // Only an async-signal-safe immediate exit runs in the forked child.
        // The parent explicitly owns/reaps this PID through the production helper.
        let selected = unsafe { libc::fork() };
        assert!(selected >= 0);
        if selected == 0 {
            unsafe {
                libc::_exit(37);
            }
        }
        let mut sibling = Command::new("/bin/sh")
            .env_clear()
            .args(["-c", "exit 41"])
            .spawn()
            .unwrap();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let result = loop {
            if let Some(code) = poll_owned_child(selected).unwrap() {
                break code;
            }
            if std::time::Instant::now() >= until {
                unsafe {
                    libc::kill(selected, libc::SIGKILL);
                    libc::waitpid(selected, std::ptr::null_mut(), 0);
                }
                panic!("specific child did not exit");
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert_eq!(result, 37);
        assert_eq!(sibling.wait().unwrap().code(), Some(41));
    }

    fn packet(token: &str) -> Vec<u8> {
        let message = StartMessage {
            token: token.into(),
            launch_config: LaunchConfig {
                attempt_id: "test".into(),
                network: NetworkPolicy::Provider,
                workspace: "/workspace".into(),
                program: "/bin/true".into(),
                args: vec![],
                environment: BTreeMap::new(),
                mounts: vec![],
            },
        };
        let bytes = serde_json::to_vec(&message).unwrap();
        let mut packet = (bytes.len() as u32).to_be_bytes().to_vec();
        packet.extend(bytes);
        packet
    }

    #[test]
    fn boundary_start_gate_requires_complete_frame_before_launch() {
        let packet = packet("correct");
        for length in 0..packet.len() {
            assert!(
                read_start(&mut &packet[..length], "correct").is_err(),
                "accepted prefix {length}"
            );
        }
        assert!(read_start(&mut packet.as_slice(), "correct").is_ok());
    }

    #[test]
    fn boundary_start_gate_rejects_wrong_positive_token() {
        assert!(matches!(
            read_start(&mut packet("wrong").as_slice(), "correct"),
            Err(Error::Conflict(_))
        ));
    }
}
