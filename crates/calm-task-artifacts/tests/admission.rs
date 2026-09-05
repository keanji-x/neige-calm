#![cfg(target_os = "linux")]
mod support;
use calm_task_artifacts::{ArtifactStore, CaptureRequest, Error, GitConfig, QuiescentSource};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    time::{Duration, Instant},
};
use support::*;

#[test]
fn linked_git_worktree_excludes_metadata_and_keeps_untracked_work() {
    let rig = Rig::new();
    git(
        &rig.source,
        &[
            "-c",
            "user.name=Artifact test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "test",
        ],
    );
    let linked = rig.destination("linked");
    git(
        &rig.source,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            linked.to_str().unwrap(),
        ],
    );
    assert!(linked.join(".git").is_file());
    fs::write(linked.join("notes"), b"linked untracked bytes").unwrap();
    let receipt = rig
        .store
        .capture(CaptureRequest {
            key: "linked",
            source: QuiescentSource {
                root: &linked,
                boundary_id: "stopped",
            },
            outputs: &[],
        })
        .unwrap();
    rig.store
        .materialize_candidate(&receipt.snapshot, &rig.destination("prepared"))
        .unwrap();
    assert_eq!(
        fs::read(rig.destination("prepared/notes")).unwrap(),
        b"linked untracked bytes"
    );
    assert!(!rig.destination("prepared/.git").exists());
}

#[test]
fn git_index_child_receives_only_the_explicit_environment() {
    let rig = Rig::new();
    let binary = rig.destination("inspect-git");
    let captured_env = rig.destination("child.env");
    fs::write(
        &binary,
        format!(
            "#!/bin/sh\n/usr/bin/env > '{}'\nexec /usr/bin/git \"$@\"\n",
            captured_env.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    let store = ArtifactStore::open(
        &rig.root,
        limits(),
        GitConfig {
            binary,
            timeout: Duration::from_secs(5),
        },
    )
    .unwrap();
    store
        .capture(CaptureRequest {
            key: "env",
            source: QuiescentSource {
                root: &rig.source,
                boundary_id: "stopped",
            },
            outputs: &[],
        })
        .unwrap();
    let environment = fs::read_to_string(captured_env).unwrap();
    let expected = [
        "PATH",
        "LC_ALL",
        "GIT_CONFIG_NOSYSTEM",
        "GIT_CONFIG_SYSTEM",
        "GIT_CONFIG_GLOBAL",
        "GIT_TERMINAL_PROMPT",
        "GIT_OPTIONAL_LOCKS",
        "GIT_NO_LAZY_FETCH",
        "GIT_ALLOW_PROTOCOL",
        "PWD",
    ];
    for line in environment.lines() {
        let (name, _) = line.split_once('=').unwrap();
        assert!(
            expected.contains(&name),
            "unexpected inherited environment key: {name}"
        );
    }
    assert!(
        environment
            .lines()
            .any(|l| l == "GIT_CONFIG_GLOBAL=/dev/null")
    );
    assert!(environment.lines().any(|l| l == "GIT_ALLOW_PROTOCOL="));
}

#[test]
fn git_index_inspection_timeout_reaps_the_child_and_publishes_nothing() {
    let rig = Rig::new();
    let binary = rig.destination("slow-git");
    let pid_file = rig.destination("child.pid");
    fs::write(
        &binary,
        format!(
            "#!/bin/sh\nprintf '%s' \"$$\" > '{}'\nexec /usr/bin/sleep 60\n",
            pid_file.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    let store = ArtifactStore::open(
        &rig.root,
        limits(),
        GitConfig {
            binary,
            timeout: Duration::from_millis(250),
        },
    )
    .unwrap();
    let start = Instant::now();
    let result = store.capture(CaptureRequest {
        key: "timeout",
        source: QuiescentSource {
            root: &rig.source,
            boundary_id: "stopped",
        },
        outputs: &[],
    });
    assert!(matches!(result, Err(Error::Unsupported(_))));
    assert!(start.elapsed() < Duration::from_secs(5));
    let pid: i32 = fs::read_to_string(pid_file).unwrap().parse().unwrap();
    assert!(pid > 0);
    // SAFETY: signal 0 only queries the status of the exact test child PID.
    assert_eq!(unsafe { nix::libc::kill(pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(nix::libc::ESRCH)
    );
    assert_eq!(fs::read_dir(rig.root.join("snapshots")).unwrap().count(), 0);
    assert_eq!(fs::read_dir(rig.root.join("captures")).unwrap().count(), 0);
}

#[test]
fn git_index_output_has_a_hard_byte_bound() {
    let rig = Rig::new();
    let binary = rig.destination("verbose-git");
    fs::write(&binary, b"#!/bin/sh\nexec /usr/bin/yes index-record\n").unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    let mut small = limits();
    small.max_manifest_bytes = 1024;
    let store = ArtifactStore::open(
        &rig.root,
        small,
        GitConfig {
            binary,
            timeout: Duration::from_secs(5),
        },
    )
    .unwrap();
    assert!(matches!(
        store.capture(CaptureRequest {
            key: "bytes",
            source: QuiescentSource {
                root: &rig.source,
                boundary_id: "stopped"
            },
            outputs: &[]
        }),
        Err(Error::Limit(_))
    ));
    assert_eq!(fs::read_dir(rig.root.join("captures")).unwrap().count(), 0);
}

#[test]
fn absolute_path_dot_segments_are_refused_without_remapping_destination() {
    let rig = Rig::new();
    rig.write("value", b"retained");
    let receipt = rig.capture("one", &[]);
    let unusual = rig.destination("new/.");
    assert!(
        rig.store
            .materialize_candidate(&receipt.snapshot, &unusual)
            .is_err()
    );
    assert!(!rig.destination("new").exists());
    assert!(ArtifactStore::open(&rig.destination("store/."), limits(), git_config()).is_err());
    let unrelated = rig.destination("unrelated");
    fs::create_dir(&unrelated).unwrap();
    fs::set_permissions(&unrelated, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(unrelated.join("valuable"), b"untouched").unwrap();
    assert!(ArtifactStore::open(&unrelated, limits(), git_config()).is_err());
    assert_eq!(fs::read_dir(&unrelated).unwrap().count(), 1);
}
