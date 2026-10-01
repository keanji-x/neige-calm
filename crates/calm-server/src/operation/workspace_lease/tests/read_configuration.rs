use super::*;

#[tokio::test]
async fn read_tree_rejects_project_permission_configuration() {
    let root = tempfile::tempdir().unwrap();
    init_git_repo(root.path());
    std::fs::create_dir(root.path().join(".codex")).unwrap();
    std::fs::write(
        root.path().join(".codex/config.toml"),
        "[permissions.project]\nfilesystem = { ':root' = 'read' }\n",
    )
    .unwrap();
    run_git(root.path(), ["add", ".codex/config.toml"]);
    run_git(root.path(), ["commit", "-m", "project configuration"]);
    assert!(
        task_guard::verify_read_tree(root.path()).await.is_err(),
        "read task cannot inherit a project layer that can replace its execution permissions"
    );
}
