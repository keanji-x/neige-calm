//! Read-only Linux restart observations. These facts never restore signal authority.
use std::path::Path;
use workspace::execution::{LinuxProcessIdentity, NativeProcessIdentity, ScopeMechanismIdentity};

pub(super) fn quiescent(
    process: &NativeProcessIdentity,
    mechanism: &ScopeMechanismIdentity,
) -> bool {
    let (NativeProcessIdentity::LinuxHelper(identity), ScopeMechanismIdentity::LinuxHelper(helper)) =
        (process, mechanism)
    else {
        return false;
    };
    if helper.protocol_version != 1 || identity.pid == 0 || identity.start_time_ticks == 0 {
        return false;
    }
    let Ok(mountinfo) = std::fs::read_to_string("/proc/self/mountinfo") else {
        return false;
    };
    observe(Path::new("/proc"), &mountinfo, identity).unwrap_or(false)
}

pub(super) fn start_time(pid: u32) -> anyhow::Result<u64> {
    Ok(stat(&std::fs::read_to_string(format!("/proc/{pid}/stat"))?)?.start)
}

struct Stat {
    pid: u32,
    pgid: u32,
    session: u32,
    start: u64,
}
fn stat(bytes: &str) -> anyhow::Result<Stat> {
    let (prefix, tail) = bytes
        .rsplit_once(')')
        .ok_or_else(|| anyhow::anyhow!("invalid process stat"))?;
    let (pid, _) = prefix
        .split_once(" (")
        .ok_or_else(|| anyhow::anyhow!("invalid process stat pid"))?;
    let fields: Vec<_> = tail.split_whitespace().collect();
    anyhow::ensure!(
        fields.len() >= 20 && fields[0].len() == 1 && fields[0].as_bytes()[0].is_ascii_alphabetic(),
        "truncated process stat"
    );
    // All remaining public stat fields are numeric; malformed partial observations prove nothing.
    for field in &fields[1..] {
        let _ = field.parse::<i128>()?;
    }
    Ok(Stat {
        pid: pid.parse()?,
        pgid: fields[2].parse()?,
        session: fields[3].parse()?,
        start: fields[19].parse()?,
    })
}

// mountinfo's mount and superblock options both describe the visible procfs view.
// Unknown syntax or stacked /proc mounts cannot establish complete enumeration.
fn qualify_mount(mountinfo: &str) -> anyhow::Result<()> {
    let mut found = false;
    for line in mountinfo.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        let separator = fields
            .iter()
            .position(|field| *field == "-")
            .ok_or_else(|| anyhow::anyhow!("missing mountinfo separator"))?;
        anyhow::ensure!(
            separator >= 6 && fields.len() == separator + 4,
            "invalid mountinfo fields"
        );
        let _: u64 = fields[0].parse()?;
        let _: u64 = fields[1].parse()?;
        let (major, minor) = fields[2]
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("invalid mountinfo device"))?;
        let _: u32 = major.parse()?;
        let _: u32 = minor.parse()?;
        anyhow::ensure!(fields[4].starts_with('/'), "invalid mountinfo path");
        if fields[4] != "/proc" {
            continue;
        }
        anyhow::ensure!(
            !found && fields[separator + 1] == "proc",
            "ambiguous proc mount"
        );
        found = true;
        for option in fields[5].split(',').chain(fields[separator + 3].split(',')) {
            let (key, value) = option.split_once('=').unwrap_or((option, ""));
            anyhow::ensure!(
                key != "hidepid" || matches!(value, "0" | "off"),
                "restricted proc mount"
            );
        }
    }
    anyhow::ensure!(found, "missing proc mount");
    Ok(())
}

fn observe(root: &Path, mountinfo: &str, identity: &LinuxProcessIdentity) -> anyhow::Result<bool> {
    qualify_mount(mountinfo)?;
    // A procfs mounted for an ancestor PID namespace exposes different numeric identities.
    anyhow::ensure!(
        std::fs::read_link(root.join("self"))? == Path::new(&std::process::id().to_string()),
        "foreign proc PID namespace"
    );
    match std::fs::read_to_string(root.join(identity.pid.to_string()).join("stat")) {
        Ok(bytes) => {
            let current = stat(&bytes)?;
            anyhow::ensure!(current.pid == identity.pid, "ambiguous process identity");
            if current.start == identity.start_time_ticks {
                return Ok(false);
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("invalid proc entry"))?;
        if !name.bytes().all(|c| c.is_ascii_digit()) || name.is_empty() {
            continue;
        }
        let pid: u32 = name.parse()?;
        // A disappearing entry is uncertainty too: retry on the next existing Inspect cycle.
        let current = stat(&std::fs::read_to_string(entry.path().join("stat"))?)?;
        anyhow::ensure!(current.pid == pid, "ambiguous proc entry");
        if current.pgid == identity.pid {
            return Ok(false);
        }
        if current.session == identity.pid {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    const MOUNTINFO: &str = "23 1 0:5 / /proc rw,nosuid,nodev,noexec - proc proc rw";

    fn proc_root() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(std::process::id().to_string(), root.path().join("self"))
            .unwrap();
        root
    }

    fn empty_view(root: &Path, mountinfo: &str) -> bool {
        observe(
            root,
            mountinfo,
            &LinuxProcessIdentity {
                pid: 42,
                start_time_ticks: 100,
                nonce: workspace::execution::CapabilityToken::fresh().unwrap(),
            },
        )
        .unwrap_or(false)
    }

    macro_rules! restricted_view {
        ($name:ident, $option:literal) => {
            #[test]
            fn $name() {
                let root = proc_root();
                assert!(!empty_view(
                    root.path(),
                    &format!("{MOUNTINFO},hidepid={}", $option)
                ));
            }
        };
    }
    restricted_view!(recovery_3192_hidepid_2, "2");
    restricted_view!(recovery_3192_hidepid_invisible, "invisible");
    restricted_view!(recovery_3192_hidepid_4, "4");
    restricted_view!(recovery_3192_hidepid_ptraceable, "ptraceable");
    restricted_view!(recovery_3192_hidepid_1, "1");
    restricted_view!(recovery_3192_hidepid_noaccess, "noaccess");
    restricted_view!(recovery_3192_hidepid_unknown, "future");
    restricted_view!(recovery_3192_hidepid_empty, "");

    #[test]
    fn recovery_3192_unrestricted_proc_view() {
        let root = proc_root();
        for options in ["", ",hidepid=0", ",hidepid=off", ",subset=pid"] {
            assert!(empty_view(root.path(), &format!("{MOUNTINFO}{options}")));
        }
    }

    #[test]
    fn recovery_3192_no_proc_mount() {
        let root = proc_root();
        for text in [
            "",
            "23 1 0:5 / /other rw - proc proc rw",
            "23 1 0:5 / /proc rw - tmpfs tmpfs rw",
        ] {
            assert!(!empty_view(root.path(), text));
        }
    }

    #[test]
    fn recovery_3192_ambiguous_proc_mount() {
        let root = proc_root();
        assert!(!empty_view(
            root.path(),
            &format!("{MOUNTINFO}\n{MOUNTINFO}")
        ));
    }

    #[test]
    fn recovery_3192_malformed_mountinfo() {
        let root = proc_root();
        for text in [
            "garbage".to_owned(),
            format!("{MOUNTINFO}\ngarbage"),
            MOUNTINFO.replace("23 1 0:5", "bad 1 0:5"),
        ] {
            assert!(!empty_view(root.path(), &text));
        }
    }

    #[test]
    fn recovery_3192_hidepid_mount_options() {
        let root = proc_root();
        assert!(!empty_view(
            root.path(),
            &MOUNTINFO.replace("rw,nosuid", "rw,hidepid=2,nosuid")
        ));
    }

    #[test]
    fn recovery_3192_wrong_pid_namespace() {
        let root = proc_root();
        std::fs::remove_file(root.path().join("self")).unwrap();
        std::os::unix::fs::symlink(
            (std::process::id() + 1).to_string(),
            root.path().join("self"),
        )
        .unwrap();
        assert!(!empty_view(root.path(), MOUNTINFO));
    }

    #[test]
    fn recovery_3192_unreadable_self() {
        let root = proc_root();
        std::fs::remove_file(root.path().join("self")).unwrap();
        assert!(!empty_view(root.path(), MOUNTINFO));
    }

    fn probe(pid: u32, pgid: u32, session: u32, start: u64) -> bool {
        probe_state(pid, pgid, session, start, "S")
    }
    fn probe_state(pid: u32, pgid: u32, session: u32, start: u64, state: &str) -> bool {
        let root = proc_root();
        std::fs::create_dir(root.path().join(pid.to_string())).unwrap();
        // /proc/<pid>/stat fields 1..22, including a comm with spaces and parentheses.
        std::fs::write(
            root.path().join(pid.to_string()).join("stat"),
            format!(
                "{pid} (test (comm)) {state} 1 {pgid} {session} 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 {start}"
            ),
        )
        .unwrap();
        observe(
            root.path(),
            MOUNTINFO,
            &LinuxProcessIdentity {
                pid: 42,
                start_time_ticks: 100,
                nonce: workspace::execution::CapabilityToken::fresh().unwrap(),
            },
        )
        .unwrap()
    }
    #[test]
    fn recovery_3192_identity_conjunct() {
        assert!(!probe(42, 99, 99, 100));
    }
    #[test]
    fn recovery_3192_zombie_identity_stays_unknown() {
        assert!(!probe_state(42, 99, 99, 100, "Z"));
    }
    #[test]
    fn recovery_3192_pgid_conjunct() {
        assert!(!probe(43, 42, 99, 200));
    }
    #[test]
    fn recovery_3192_session_conjunct() {
        assert!(!probe(43, 99, 42, 200));
    }
    #[test]
    fn recovery_3192_reused_identity_without_members_is_quiescent() {
        assert!(probe(42, 99, 99, 200));
    }
    #[test]
    fn recovery_3192_missing_or_malformed_stat_is_uncertain() {
        let root = proc_root();
        std::fs::create_dir(root.path().join("43")).unwrap();
        let identity = LinuxProcessIdentity {
            pid: 42,
            start_time_ticks: 100,
            nonce: workspace::execution::CapabilityToken::fresh().unwrap(),
        };
        assert!(observe(root.path(), MOUNTINFO, &identity).is_err());
        std::fs::write(root.path().join("43/stat"), "malformed").unwrap();
        assert!(observe(root.path(), MOUNTINFO, &identity).is_err());
    }
}
