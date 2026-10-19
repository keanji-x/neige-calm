use std::{os::unix::fs::MetadataExt, path::Path, process::Command};
#[cfg(target_os = "linux")]
#[test]
fn execution_qualification_real_namespace_mount_changes_and_fork_boundary() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("root");
    let source = temporary.path().join("source");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(root.join("mounted")).unwrap();
    let original_mountpoint = std::fs::metadata(root.join("mounted")).unwrap().ino();
    let source_inode = std::fs::metadata(&source).unwrap().ino();
    assert_ne!(original_mountpoint, source_inode);
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .arg(&root)
            .status()
            .unwrap()
            .success()
    );
    let output = Command::new(env!("CARGO_BIN_EXE_proc-supervisor-foreground-job"))
        .arg("--probe-object-qualification")
        .args([
            &root,
            &source,
            Path::new(env!("CARGO_BIN_EXE_calm-proc-supervisor")),
        ])
        .env_clear()
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "required private namespace qualification failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    for field in [
        "namespace_changed_unknown",
        "namespace_comparison_unsupported",
        "cwd_cross_mount_unsupported",
        "changed_mount_unknown",
        "lost_mount_unknown",
        "no_payload",
    ] {
        assert_eq!(report[field], true, "{field}: {report}");
    }
    assert_eq!(
        std::fs::metadata(root.join("mounted")).unwrap().ino(),
        original_mountpoint,
        "bind escaped the disposable child's namespace"
    );
    assert!(!root.join("mounted/forbidden").exists());
    assert!(
        !source.join("forbidden").exists(),
        "scratch bind must not escape its child mount namespace"
    );
}
