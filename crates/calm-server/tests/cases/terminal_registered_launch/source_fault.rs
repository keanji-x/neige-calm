//! The real kernel aborts after the workspace intent commits, before Attach.
use super::{ws::post_at, *};
use crate::support::kernel_proc::{self, ChildGuard};
use std::{ffi::OsString, os::unix::process::ExitStatusExt};

const KEY: &str = "terminal-source-intent-crash";
const SEAM: &str = "workspace-source-intent-committed";

enum KernelBoot {
    CrashAfterIntent,
    Normal,
}

async fn kernel(fixture: &Fixture, boot: KernelBoot) -> anyhow::Result<ChildGuard> {
    let root = fixture._root.path().to_owned();
    let database = root.join("server.sqlite");
    let socket = fixture._sockets.path().join("control.sock");
    let key = root.join("kernel-key.json");
    tokio::task::spawn_blocking(move || {
        let port = kernel_proc::free_port_or_skip("terminal source intent")
            .context("this boundary test requires its own loopback listener")?;
        let mut env = vec![
            ("CALM_AUTH_USERNAME", OsString::from("owner")),
            ("CALM_AUTH_PASSWORD", OsString::from("terminal-password")),
            ("CALM_DEV_AUTOLOGIN", OsString::from("false")),
            ("TOKIO_WORKER_THREADS", OsString::from("2")),
        ];
        if matches!(boot, KernelBoot::CrashAfterIntent) {
            env.push(("CALM_TEST_CRASH_AT", OsString::from(SEAM)));
        }
        let log = root.join(if matches!(boot, KernelBoot::CrashAfterIntent) {
            "source-crash.log"
        } else {
            "source-reboot.log"
        });
        let mut child = ChildGuard {
            child: kernel_proc::spawn_kernel_to_with_args(
                &root,
                &database,
                port,
                &env,
                Some(&log),
                &[
                    "--terminal-executor-sock".into(),
                    socket.into_os_string(),
                    "--terminal-executor-key-file".into(),
                    key.into_os_string(),
                ],
            ),
            port,
        };
        ensure!(
            kernel_proc::wait_ready(&mut child.child, port).is_ok(),
            "real kernel failed before HTTP was ready: {}",
            std::fs::read_to_string(&log)?
        );
        Ok(child)
    })
    .await?
}

async fn login(kernel: &ChildGuard) -> anyhow::Result<String> {
    let (status, cookie, _) = post_at(
        ([127, 0, 0, 1], kernel.port).into(),
        "/api/auth/login".into(),
        String::new(),
        json!({"username":"owner","password":"terminal-password"}),
        None,
    )
    .await?;
    ensure!(status == 200, "real kernel login failed: {status}");
    cookie.context("real kernel returned no session cookie")
}

async fn exercise(fixture: &Fixture) -> anyhow::Result<()> {
    let mut first = kernel(fixture, KernelBoot::CrashAfterIntent).await?;
    let cookie = login(&first).await?;
    let interrupted = post_at(
        ([127, 0, 0, 1], first.port).into(),
        format!("/api/tracks/{}/terminal-cards", fixture.track),
        cookie,
        fixture.request_body(),
        Some(KEY.into()),
    )
    .await;
    let first = tokio::task::spawn_blocking(move || {
        let status = kernel_proc::wait_exit_with_timeout(&mut first, Duration::from_secs(10));
        (first, status)
    })
    .await?;
    ensure!(
        first.1.signal() == Some(libc::SIGABRT),
        "first real kernel did not abort at the committed-intent seam: {:?}",
        first.1
    );
    ensure!(
        interrupted.is_err(),
        "crashed HTTP launch unexpectedly returned a result"
    );
    let crash_log = std::fs::read_to_string(fixture._root.path().join("source-crash.log"))?;
    ensure!(
        crash_log.contains(&format!("CALM_TEST_CRASH_AT={SEAM}: aborting")),
        "kernel exited at a different boundary: {crash_log}"
    );
    let (operation,request):(String,Option<String>)=sqlx::query_as("SELECT id,execution_request_json FROM operations WHERE kind='terminal-create' AND idempotency_key=?1")
        .bind(KEY).fetch_one(fixture.sql.pool()).await?;
    ensure!(
        request.is_none(),
        "first kernel crossed into request reservation before the intent seam"
    );
    let source: String =
        sqlx::query_scalar("SELECT registration_intent_json FROM workspace_execution_sources")
            .fetch_one(fixture.sql.pool())
            .await?;
    let before: calm_session::control::execution::AttachRegistration =
        serde_json::from_str(&source)?;
    ensure!(
        std::fs::read_dir(&fixture.journal)?.all(|entry| entry.is_ok_and(|entry| !entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "registration" || extension == "execution"))),
        "first kernel installed a registration or execution before its intent seam"
    );
    let mut second = kernel(fixture, KernelBoot::Normal).await?;
    let result=async {
        let cookie=login(&second).await?;
        let (status,_,card)=post_at(([127,0,0,1],second.port).into(),format!("/api/tracks/{}/terminal-cards",fixture.track),
            cookie,fixture.request_body(),Some(KEY.into())).await?;
        ensure!(status==201,"source-owner recovery could not complete the original HTTP launch after intent commit without Attach: {status} {card}");
        let terminal=fixture.sql.terminal_get_by_card(card["id"].as_str().context("recovery returned no original card")?).await?.context("original recovered Terminal missing")?;
        let (original,request):(String,String)=sqlx::query_as("SELECT o.id,o.execution_request_json FROM operations o JOIN terminals t ON t.launch_operation_id=o.id WHERE t.id=?1")
            .bind(&terminal.id).fetch_one(fixture.sql.pool()).await?;
        ensure!(original==operation,"source recovery replaced the original operation");
        let request:PrepareExecution=serde_json::from_value(serde_json::from_str::<Value>(&request)?["request"].clone())?;
        let source:String=sqlx::query_scalar("SELECT registration_intent_json FROM workspace_execution_sources").fetch_one(fixture.sql.pool()).await?;
        let after:calm_session::control::execution::AttachRegistration=serde_json::from_str(&source)?;
        ensure!(before.incarnation != after.incarnation,"recovery reopened a path under the unwitnessed source incarnation");
        ensure!(matches!((&before.registration,&after.registration),
            (calm_session::control::execution::ExecutionRegistration::Worktree {generation:previous,..},
             calm_session::control::execution::ExecutionRegistration::Worktree {generation:current,..}) if current>previous),
            "source recovery did not use an explicit higher generation");
        ensure!(request.identity.operation_id==operation,"recovered execution belongs to another operation");
        tokio::time::timeout(Duration::from_secs(5),async {
            loop {if std::fs::read_to_string(&fixture.count).ok().as_deref()==Some("executed\n") {break;}
                tokio::time::sleep(Duration::from_millis(10)).await;}
        }).await.context("recovered real program did not run")?;
        Ok(())
    }.await;
    second.sigkill_and_reap();
    result
}

#[tokio::test]
async fn terminal_source_intent_without_attach_recovers_original_http_launch_after_kernel_crash()
-> anyhow::Result<()> {
    let mut fixture = Fixture::start_in(Storage::Disk).await?;
    let result = exercise(&fixture).await;
    fixture.stop().await;
    result?;
    ensure!(
        std::fs::read_to_string(&fixture.count)? == "executed\n",
        "source intent recovery issued duplicate process effects"
    );
    Ok(())
}
