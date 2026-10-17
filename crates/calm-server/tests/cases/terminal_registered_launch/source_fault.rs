//! Actual kernel crashes around source retention and its one-shot request.
use super::{ws::post_at, *};
use crate::support::kernel_proc::{self, ChildGuard};
use std::{ffi::OsString, os::unix::process::ExitStatusExt};

const KEY: &str = "terminal-source-intent-crash";
#[derive(Clone, Copy)]
enum CrashWindow {
    Intent,
    SourceAttached,
    RequestReserved,
}
impl CrashWindow {
    fn seam(self) -> &'static str {
        match self {
            Self::Intent => "workspace-source-intent-committed",
            Self::SourceAttached => "terminal-source-attached-before-request",
            Self::RequestReserved => "terminal-request-reserved-before-prepare",
        }
    }
}

enum KernelBoot {
    Crash(CrashWindow),
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
        let log = root.join(match boot {
            KernelBoot::Crash(window) => {
                env.push(("CALM_TEST_CRASH_AT", OsString::from(window.seam())));
                format!("{}.log", window.seam())
            }
            KernelBoot::Normal => "source-reboot.log".to_owned(),
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

async fn exercise(fixture: &Fixture, window: CrashWindow) -> anyhow::Result<()> {
    let seam = window.seam();
    let mut first = kernel(fixture, KernelBoot::Crash(window)).await?;
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
        "first real kernel did not abort at the specified seam: {:?}",
        first.1
    );
    ensure!(
        interrupted.is_err(),
        "crashed HTTP launch unexpectedly returned a result"
    );
    let crash_log = std::fs::read_to_string(fixture._root.path().join(format!("{seam}.log")))?;
    ensure!(
        crash_log.contains(&format!("CALM_TEST_CRASH_AT={seam}: aborting")),
        "kernel exited at a different boundary: {crash_log}"
    );
    let (operation,request):(String,Option<String>)=sqlx::query_as("SELECT id,execution_request_json FROM operations WHERE kind='terminal-create' AND idempotency_key=?1")
        .bind(KEY).fetch_one(fixture.sql.pool()).await?;
    ensure!(
        request.is_some() == matches!(window, CrashWindow::RequestReserved),
        "original request persistence crossed the wrong boundary at {seam}"
    );
    let frozen_request = request;
    let source: String =
        sqlx::query_scalar("SELECT registration_intent_json FROM workspace_execution_sources")
            .fetch_one(fixture.sql.pool())
            .await?;
    let before: calm_session::control::execution::AttachRegistration =
        serde_json::from_str(&source)?;
    let entries = std::fs::read_dir(&fixture.journal)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    ensure!(
        !entries.iter().any(|path| path
            .extension()
            .is_some_and(|extension| extension == "execution")),
        "first kernel prepared or launched a process before {seam}"
    );
    let retained = entries.iter().any(|path| {
        path.extension()
            .is_some_and(|extension| extension == "registration")
    });
    ensure!(
        retained != matches!(window, CrashWindow::Intent),
        "actual source retention crossed the wrong boundary at {seam}"
    );
    let mut second = kernel(fixture, KernelBoot::Normal).await?;
    let result=async {
        let cookie=login(&second).await?;
        let (status,_,card)=post_at(([127,0,0,1],second.port).into(),format!("/api/tracks/{}/terminal-cards",fixture.track),
            cookie,fixture.request_body(),Some(KEY.into())).await?;
        ensure!(status==201,"original HTTP launch could not recover after {seam}: {status} {card}");
        let terminal=fixture.sql.terminal_get_by_card(card["id"].as_str().context("recovery returned no original card")?).await?.context("original recovered Terminal missing")?;
        let (original,request):(String,String)=sqlx::query_as("SELECT o.id,o.execution_request_json FROM operations o JOIN terminals t ON t.launch_operation_id=o.id WHERE t.id=?1")
            .bind(&terminal.id).fetch_one(fixture.sql.pool()).await?;
        ensure!(original==operation,"source recovery replaced the original operation");
        if let Some(frozen)=&frozen_request {ensure!(&request==frozen,"recovery changed the immutable original request after {seam}");}
        let request:PrepareExecution=serde_json::from_value(serde_json::from_str::<Value>(&request)?["request"].clone())?;
        let source:String=sqlx::query_scalar("SELECT registration_intent_json FROM workspace_execution_sources").fetch_one(fixture.sql.pool()).await?;
        let after:calm_session::control::execution::AttachRegistration=serde_json::from_str(&source)?;
        if matches!(window,CrashWindow::Intent) {
        ensure!(before.incarnation != after.incarnation,"recovery reopened a path under the unwitnessed source incarnation");
        ensure!(matches!((&before.registration,&after.registration),
            (calm_session::control::execution::ExecutionRegistration::Worktree {generation:previous,..},
             calm_session::control::execution::ExecutionRegistration::Worktree {generation:current,..}) if current>previous),
            "source recovery did not use an explicit higher generation");
        } else {ensure!(source==serde_json::to_string(&before)?,"recovery replaced an already-retained actual source after {seam}");}
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

async fn run(window: CrashWindow) -> anyhow::Result<()> {
    let mut fixture = Fixture::start_in(Storage::Disk).await?;
    let result = exercise(&fixture, window).await;
    fixture.stop().await;
    if result.is_err() {
        for name in [
            format!("{}.log", window.seam()),
            "source-reboot.log".to_owned(),
        ] {
            if let Ok(log) = std::fs::read_to_string(fixture._root.path().join(&name)) {
                eprintln!("actual kernel {name}:\n{log}");
            }
        }
    }
    result?;
    ensure!(
        std::fs::read_to_string(&fixture.count)? == "executed\n",
        "source/request recovery issued duplicate process effects"
    );
    Ok(())
}

#[tokio::test]
async fn terminal_source_intent_without_attach_recovers_original_http_launch_after_kernel_crash()
-> anyhow::Result<()> {
    run(CrashWindow::Intent).await
}

#[tokio::test]
async fn terminal_attached_source_before_request_recovers_original_http_launch_after_kernel_crash()
-> anyhow::Result<()> {
    run(CrashWindow::SourceAttached).await
}

#[tokio::test]
async fn terminal_original_request_before_prepare_recovers_same_ticket_after_kernel_crash()
-> anyhow::Result<()> {
    run(CrashWindow::RequestReserved).await
}

#[tokio::test]
async fn terminal_second_launch_interruption_has_no_unrecorded_live_registration()
-> anyhow::Result<()> {
    use calm_session::control::execution::{
        AttachRegistration, ExportRegistration, ObjectRegistrationReceipt, RegistrationExport,
        TransferNonce,
    };
    let mut fixture = Fixture::start_in(Storage::Disk).await?;
    let result = async {
        fixture.post_terminal(Some("first-source-anchor")).await?;
        let mut interrupted = kernel(&fixture, KernelBoot::Crash(CrashWindow::SourceAttached)).await?;
        let cookie = login(&interrupted).await?;
        let response = post_at(
            ([127, 0, 0, 1], interrupted.port).into(),
            format!("/api/tracks/{}/terminal-cards", fixture.track),
            cookie,
            fixture.request_body(),
            Some(KEY.into()),
        ).await;
        let status = kernel_proc::wait_exit_with_timeout(&mut interrupted, Duration::from_secs(10));
        ensure!(status.signal() == Some(libc::SIGABRT) && response.is_err(),
            "second launch did not stop at the actual source-attachment boundary: {status:?}");
        let anchor: String = sqlx::query_scalar("SELECT registration_intent_json FROM workspace_execution_sources")
            .fetch_one(fixture.sql.pool()).await?;
        let anchor: AttachRegistration = serde_json::from_str(&anchor)?;
        let requests: Vec<String> = sqlx::query_scalar("SELECT execution_request_json FROM operations WHERE execution_request_json IS NOT NULL")
            .fetch_all(fixture.sql.pool()).await?;
        let mut owned = vec![anchor.registration_id];
        for request in requests {
            let request: Value = serde_json::from_str(&request)?;
            let transfer: AttachRegistration = serde_json::from_value(request["registration"].clone())?;
            owned.push(transfer.registration_id);
        }
        // Audit receipts survive release. Only a successful exact Export proves
        // that the still-running executor actually retains a native lease.
        let secret = serde_json::from_slice(&std::fs::read(fixture._root.path().join("kernel-key.json"))?)?;
        let mut observer = RegisteredExecutorConnection::connect_with_query(
            fixture._sockets.path().join("control.sock"), secret).await?;
        let mut unowned = Vec::new();
        for entry in std::fs::read_dir(&fixture.journal)? {
            let path = entry?.path();
            if path.extension().is_none_or(|extension| extension != "registration") { continue; }
            let receipt: ObjectRegistrationReceipt = serde_json::from_slice(&std::fs::read(path)?)?;
            if matches!(observer.export(ExportRegistration {
                executor_boot: observer.executor_boot().clone(),
                registration: receipt.id.clone(), nonce: TransferNonce::fresh()?,
            }).await?, RegistrationExport::Held { .. }) && !owned.contains(&receipt.id) {
                unowned.push(receipt.id);
            }
        }
        ensure!(unowned.is_empty(), "second launch installed unrecorded live registrations: {unowned:?}");
        let mut resumed = kernel(&fixture, KernelBoot::Normal).await?;
        let resumed_result = async {
            let cookie = login(&resumed).await?;
            let (status, _, card) = post_at(
                ([127, 0, 0, 1], resumed.port).into(),
                format!("/api/tracks/{}/terminal-cards", fixture.track), cookie,
                fixture.request_body(), Some(KEY.into()),
            ).await?;
            ensure!(status == 201, "second original launch did not resume: {status} {card}");
            tokio::time::timeout(Duration::from_secs(5), async {
                while std::fs::read_to_string(&fixture.count).ok().as_deref() != Some("executed\nexecuted\n") {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }).await.context("resumed second launch did not execute exactly once")?;
            Ok::<_, anyhow::Error>(())
        }.await;
        resumed.sigkill_and_reap();
        resumed_result
    }.await;
    fixture.stop().await;
    result
}
