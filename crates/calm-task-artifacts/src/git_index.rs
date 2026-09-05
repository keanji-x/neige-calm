//! Read-only index admission. No file bytes come from Git, so filters cannot
//! alter artifacts. `--sparse` avoids expanding sparse trees (which we refuse).
use crate::{Error, GitConfig, Limits, Result};
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use std::{
    io::Read,
    os::fd::AsRawFd,
    path::Path,
    process::{Child, Command, Stdio},
    time::Instant,
};

struct GitChild(Child);
impl Drop for GitChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub(crate) fn inspect(source: &Path, config: &GitConfig, limits: &Limits) -> Result<()> {
    let child = Command::new(&config.binary)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_ALLOW_PROTOCOL", "")
        .args([
            "--no-optional-locks",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.untrackedCache=false",
        ])
        .arg("-C")
        .arg(source)
        .args(["ls-files", "--stage", "--sparse", "-z"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut child = GitChild(child);
    let mut stdout = child
        .0
        .stdout
        .take()
        .ok_or_else(|| Error::Integrity("Git stdout missing".into()))?;
    fcntl(stdout.as_raw_fd(), FcntlArg::F_SETFL(OFlag::O_NONBLOCK))
        .map_err(std::io::Error::from)?;
    let start = Instant::now();
    let mut data = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut eof = false;
    loop {
        if start.elapsed() >= config.timeout {
            return Err(Error::Unsupported("Git index inspection timed out".into()));
        }
        match stdout.read(&mut buffer) {
            Ok(0) => eof = true,
            Ok(n) => {
                if (data.len() as u64).saturating_add(n as u64) > limits.max_manifest_bytes {
                    return Err(Error::Limit("Git index listing".into()));
                }
                data.extend_from_slice(&buffer[..n]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e.into()),
        }
        if eof && let Some(status) = child.0.try_wait()? {
            if !status.success() {
                return Err(Error::Unsupported("Git index inspection failed".into()));
            }
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let mut count = 0usize;
    for record in data.split_inclusive(|b| *b == 0) {
        count += 1;
        if count > limits.max_entries {
            return Err(Error::Limit("tracked index entries".into()));
        }
        if record.last() != Some(&0) {
            return Err(Error::Integrity("unterminated Git index record".into()));
        }
        let record = &record[..record.len() - 1];
        let tab = record
            .iter()
            .position(|b| *b == b'\t')
            .ok_or_else(|| Error::Integrity("Git index record".into()))?;
        let header = std::str::from_utf8(&record[..tab])
            .map_err(|_| Error::Unsupported("Git index encoding".into()))?;
        let fields = header.split(' ').collect::<Vec<_>>();
        if fields.len() != 3 || !matches!(fields[0], "100644" | "100755") {
            return Err(Error::Unsupported(
                "tracked symlink, submodule, sparse tree or index mode".into(),
            ));
        }
        let path = std::str::from_utf8(&record[tab + 1..])
            .map_err(|_| Error::Unsupported("non-UTF-8 index path".into()))?;
        crate::model::path(path, limits)?;
    }
    Ok(())
}
