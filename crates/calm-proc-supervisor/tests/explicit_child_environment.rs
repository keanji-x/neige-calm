use calm_session::control::{AttachRequest, ControlMsg, ControlReply, EnsureProcRequest, IoMode};
use calm_session::{read_frame, write_frame};
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;

#[tokio::test]
async fn pty_children_exclude_application_credentials_but_keep_explicit_environment()
-> anyhow::Result<()> {
    let root = calm_test_sockets::socket_dir("env-fence");
    let socket = calm_test_sockets::socket_path(root.path(), "supervisor.sock");
    let mut supervisor = tokio::process::Command::new(env!("CARGO_BIN_EXE_calm-proc-supervisor"))
        .args(["--control-sock", socket.to_str().unwrap()])
        .env("NEIGE_PRIVATE_APPLICATION_CREDENTIAL", "must-not-reach-pty")
        .env("HOME", root.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut connection = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(stream) = UnixStream::connect(&socket).await {
                return stream;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    write_frame(&mut connection,&ControlMsg::EnsureProc(EnsureProcRequest {
        proc_id:"env-check".into(),program:"/bin/sh".into(),
        args:vec!["-c".into(),"printf 'credential=%s\\nhome=%s\\nexplicit=%s\\n' \"${NEIGE_PRIVATE_APPLICATION_CREDENTIAL-absent}\" \"$HOME\" \"$EXPLICIT_VALUE\"".into()],
        envs:vec![("EXPLICIT_VALUE".into(),"intentional".into())], cwd:root.path().to_str().unwrap().into(),
        ready_timeout_ms:5000,io_mode:IoMode::Pty { cols:80,rows:24 }, replay_bytes:8192,
    })).await?;
    assert!(matches!(
        read_frame::<ControlReply, _>(&mut connection).await?,
        ControlReply::Spawned { .. }
    ));
    assert!(matches!(
        read_frame::<ControlReply, _>(&mut connection).await?,
        ControlReply::Ready
    ));
    write_frame(
        &mut connection,
        &ControlMsg::Attach(AttachRequest {
            proc_id: "env-check".into(),
            from_cursor: None,
            reader_id: "env-probe".into(),
        }),
    )
    .await?;
    let output = tokio::time::timeout(Duration::from_secs(5), async {
        let mut output = Vec::new();
        loop {
            match read_frame::<ControlReply, _>(&mut connection).await? {
                ControlReply::AttachOk(attached) => output.extend(attached.replay),
                ControlReply::Output { bytes, .. } => output.extend(bytes),
                ControlReply::Exited { .. } => return Ok::<_, anyhow::Error>(output),
                other => anyhow::bail!("unexpected supervisor reply {other:?}"),
            }
        }
    })
    .await??;
    supervisor.kill().await?;
    let text = String::from_utf8(output)?;
    assert!(
        text.contains("credential=absent"),
        "application environment escaped: {text:?}"
    );
    assert!(text.contains(&format!("home={}", root.path().display())));
    assert!(text.contains("explicit=intentional"));
    Ok(())
}
