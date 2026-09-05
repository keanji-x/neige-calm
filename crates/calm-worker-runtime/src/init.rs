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
    // waitpid(-1) also reaps adopted background children. This process exits
    // when the provider exits; Linux then terminates all remaining descendants.
    drop(child);
    loop {
        let mut status = 0;
        let pid = unsafe { libc::waitpid(-1, &mut status, 0) };
        if pid < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        if pid == provider_pid {
            let code = if libc::WIFEXITED(status) {
                libc::WEXITSTATUS(status)
            } else {
                128 + libc::WTERMSIG(status)
            };
            std::process::exit(code);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

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
