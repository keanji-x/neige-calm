use super::*;
use std::{
    collections::BTreeMap,
    io::Write,
    process::{Command, Stdio},
};

#[tokio::test]
async fn workspace_private_release_reaches_child_without_persisting_values() {
    std::fs::create_dir_all("_local/tmp").unwrap();
    let root = tempfile::tempdir_in("_local/tmp").unwrap();
    let root_path = std::fs::canonicalize(root.path()).unwrap();
    let logs = root_path.join("logs");
    std::fs::create_dir(&logs).unwrap();
    let secret = "synthetic-private\n$(touch injected)\n";
    let release = Release::with_private(
        vec![
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("HOME".into(), root_path.as_os_str().into()),
        ],
        BTreeMap::from([
            ("PROJECT_SECRET".into(), secret.into()),
            ("EMPTY_VALUE".into(), "".into()),
        ]),
        Some("synthetic-mcp".into()),
        "test",
    )
    .unwrap();
    let id = ExecutionId::fresh().unwrap();
    let intent = wrapped_intent(
        LaunchOwner { consumer: "test".into(), alias: ProcessAlias("test:launch".into()), owner: OperationRef("test:owner".into()), scope: ExecutionOwnership::Track(TrackId("test-track".into())) },
        &root_path, &id, &logs, Path::new("/bin/sh"),
        vec!["-c".into(), "printf '%s' \"${PROJECT_SECRET-unset}\" > result; test \"${EMPTY_VALUE-unset}\" = ''; test -n \"$NEIGE_MCP_TOKEN\"".into()],
        release.public, &release.read_prefix,
        calm_session::control::execution::AmbientEnvironment::None,
    ).await.unwrap();
    let durable = serde_json::to_string(&intent.metadata).unwrap();
    assert!(!durable.contains(secret));
    assert!(!durable.contains("synthetic-mcp"));
    let mut child = Command::new(&intent.metadata.program.program)
        .args(&intent.metadata.program.args)
        .env_clear()
        .envs(intent.metadata.program.environment)
        .current_dir(&root_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&release.payload)
        .unwrap();
    assert!(child.wait_with_output().unwrap().status.success());
    assert_eq!(
        std::fs::read_to_string(root_path.join("result")).unwrap(),
        secret
    );
    assert!(!root_path.join("injected").exists());
}

#[test]
fn workspace_private_release_refuses_oversize_without_exposing_values() {
    let error = Release::with_private(
        vec![],
        BTreeMap::from([(
            "PROJECT_SECRET".into(),
            "synthetic-oversize".repeat(2048).into(),
        )]),
        None,
        "test",
    )
    .err()
    .expect("oversize private environment must fail");
    assert!(matches!(error, CalmError::BadRequest(_)));
    assert!(!format!("{error:?}").contains("synthetic-oversize"));
}

#[test]
fn workspace_private_release_validates_keys_before_generating_shell() {
    let error = Release::with_private(
        vec![],
        BTreeMap::from([("BAD;exit".into(), "synthetic-invalid".into())]),
        None,
        "test",
    )
    .err()
    .expect("invalid private environment key must fail");
    assert!(matches!(error, CalmError::BadRequest(_)));
    assert!(!format!("{error:?}").contains("synthetic-invalid"));
}
