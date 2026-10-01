//! Explicit Git snapshot data for protocol tests that exercise real read capabilities.

pub(super) fn pinned_read_workspace() -> (tempfile::TempDir, String, std::path::PathBuf) {
    let workspace = tempfile::tempdir().unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "snapshot",
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .args(args)
                .current_dir(workspace.path())
                .status()
                .unwrap()
                .success()
        );
    }
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(workspace.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let sha = String::from_utf8(output.stdout).unwrap().trim().to_owned();
    let common = std::fs::canonicalize(workspace.path().join(".git")).unwrap();
    (workspace, sha, common)
}
