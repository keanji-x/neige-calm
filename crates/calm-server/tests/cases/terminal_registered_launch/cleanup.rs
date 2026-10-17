//! Actual product deletion releases native storage after retaining ended screens.
use super::{ws::*, *};
use calm_session::control::execution::{
    AttachRegistration, ExportRegistration, RegistrationExport, TransferNonce,
};
use std::os::unix::fs::MetadataExt;

async fn delete(server: &HttpServer, cookie: &str, path: String) -> anyhow::Result<(u16, String)> {
    let address = server.address;
    let cookie = cookie.to_owned();
    tokio::task::spawn_blocking(move || {
        let response = match ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(8))
            .build()
            .delete(&format!("http://{address}{path}"))
            .set("cookie", &cookie)
            .call()
        {
            Ok(response) | Err(ureq::Error::Status(_, response)) => response,
            Err(error) => return Err(anyhow::Error::new(error)),
        };
        Ok((response.status(), response.into_string()?))
    })
    .await?
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct NativeResources {
    roots: usize,
    ptys: usize,
    pidfds: usize,
}

fn native_resources(fixture: &Fixture) -> anyhow::Result<NativeResources> {
    let root = std::fs::metadata(&fixture.cwd)?;
    let mut result = NativeResources::default();
    for entry in std::fs::read_dir(format!("/proc/{}/fd", fixture.supervisor.child.id()))? {
        let path = entry?.path();
        if let Ok(metadata) = std::fs::metadata(&path)
            && metadata.dev() == root.dev()
            && metadata.ino() == root.ino()
        {
            result.roots += 1;
        }
        if let Ok(target) = std::fs::read_link(path) {
            if target == Path::new("anon_inode:[pidfd]") {
                result.pidfds += 1;
            }
            if target == Path::new("/dev/pts/ptmx") {
                result.ptys += 1;
            }
        }
    }
    Ok(result)
}

async fn observer(fixture: &Fixture) -> anyhow::Result<RegisteredExecutorConnection> {
    let secret = serde_json::from_slice(&std::fs::read(
        fixture._root.path().join("kernel-key.json"),
    )?)?;
    Ok(RegisteredExecutorConnection::connect_with_query(
        fixture._sockets.path().join("control.sock"),
        secret,
    )
    .await?)
}

async fn export(
    client: &mut RegisteredExecutorConnection,
    transfer: &AttachRegistration,
) -> anyhow::Result<RegistrationExport> {
    Ok(client
        .export(ExportRegistration {
            executor_boot: client.executor_boot().clone(),
            registration: transfer.registration_id.clone(),
            nonce: TransferNonce::fresh()?,
        })
        .await?)
}

async fn wait_resources(fixture: &Fixture, expected: NativeResources) -> anyhow::Result<()> {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let actual = native_resources(fixture)?;
            if actual == expected {
                break Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .with_context(|| {
        format!(
            "native storage did not return to {expected:?}; actual {:?}",
            native_resources(fixture)
        )
    })??;
    Ok(())
}

async fn exercise(fixture: &Fixture, server: &HttpServer) -> anyhow::Result<()> {
    let cookie = login(server).await?;
    let initial = native_resources(fixture)?;
    let mut client = observer(fixture).await?;
    let mut anchor = None;
    for index in 0..3 {
        let (status, _, card) = post_at(
            server.address,
            format!("/api/tracks/{}/terminal-cards", fixture.track),
            cookie.clone(),
            fixture.request_body(),
            Some(format!("released-terminal-{index}")),
        )
        .await?;
        ensure!(status == 201, "product create failed: {status} {card}");
        let terminal = fixture
            .sql
            .terminal_get_by_card(card["id"].as_str().context("missing card")?)
            .await?
            .context("missing created Terminal")?;
        let entry = fixture
            .state
            .terminal_renderer
            .get(&terminal.id)
            .context("missing renderer")?;
        ensure!(
            entry
                .wait_exit_persisted_for_test(Duration::from_secs(5))
                .await,
            "real process did not complete through the renderer"
        );
        let frame = entry.handle.model_view.lock().unwrap().capture(0)?.0;
        ensure!(
            frame
                .text
                .iter()
                .any(|line| line.contains("terminal-vertical-complete")),
            "completed Terminal lost its product screen before deletion"
        );
        drop(entry);
        let json: String = sqlx::query_scalar("SELECT o.execution_request_json FROM operations o JOIN terminals t ON t.launch_operation_id=o.id WHERE t.id=?1")
            .bind(&terminal.id).fetch_one(fixture.sql.pool()).await?;
        let request: Value = serde_json::from_str(&json)?;
        let own: AttachRegistration = serde_json::from_value(request["registration"].clone())?;
        let source: AttachRegistration = serde_json::from_value(request["source_anchor"].clone())?;
        ensure!(
            own.registration_id != source.registration_id,
            "operation reused the workspace anchor lease"
        );
        ensure!(
            matches!(
                export(&mut client, &own).await?,
                RegistrationExport::Held { .. }
            ),
            "completed history was cleaned before actual deletion"
        );
        let (status, body) =
            delete(server, &cookie, format!("/api/cards/{}", terminal.card_id)).await?;
        ensure!(status == 204, "product delete failed: {status} {body}");
        ensure!(
            matches!(
                export(&mut client, &own).await?,
                RegistrationExport::Unwitnessed
            ),
            "product deletion left the operation's actual registration lease held"
        );
        ensure!(
            matches!(
                export(&mut client, &source).await?,
                RegistrationExport::Held { .. }
            ),
            "card deletion released its workspace-owned continuity anchor"
        );
        ensure!(
            fixture.state.terminal_renderer.get(&terminal.id).is_none(),
            "deleted renderer remains installed"
        );
        wait_resources(
            fixture,
            NativeResources {
                roots: initial.roots + 1,
                ..initial
            },
        )
        .await?;
        anchor = Some(source);
    }
    let (status, body) = delete(server, &cookie, format!("/api/tracks/{}", fixture.track)).await?;
    ensure!(status == 204, "workspace delete failed: {status} {body}");
    ensure!(
        matches!(
            export(&mut client, &anchor.context("missing source anchor")?).await?,
            RegistrationExport::Unwitnessed
        ),
        "workspace deletion left its actual shared anchor held"
    );
    // Worktree deletion removes the path; use a descriptor census captured
    // before that filesystem effect to check process storage independently.
    let pid = fixture.supervisor.child.id();
    let targets = std::fs::read_dir(format!("/proc/{pid}/fd"))?
        .filter_map(Result::ok)
        .filter_map(|entry| std::fs::read_link(entry.path()).ok())
        .collect::<Vec<_>>();
    ensure!(
        !targets
            .iter()
            .any(|target| target == Path::new("anon_inode:[pidfd]")
                || target == Path::new("/dev/pts/ptmx")),
        "workspace deletion retained native process storage: {targets:?}"
    );
    Ok(())
}

#[tokio::test]
async fn terminal_product_deletion_releases_operation_storage_and_workspace_anchor()
-> anyhow::Result<()> {
    let mut fixture = Fixture::start_in(Storage::Disk).await?;
    let mut server = HttpServer::start(fixture.app.clone()).await?;
    let result = exercise(&fixture, &server).await;
    let server_result = server.stop().await;
    fixture.stop().await;
    result?;
    server_result?;
    ensure!(
        std::fs::read_to_string(&fixture.count)? == "executed\nexecuted\nexecuted\n",
        "deletion/reuse issued extra process effects"
    );
    Ok(())
}

#[tokio::test]
async fn terminal_real_output_eof_precedes_durable_drain_and_final_screen() -> anyhow::Result<()> {
    let mut fixture = Fixture::start_in(Storage::Disk).await?;
    let release = fixture._root.path().join("release-output-reader");
    fixture.supervisor.stop().await;
    let socket = fixture._sockets.path().join("control.sock");
    let key = fixture._root.path().join("kernel-key.json");
    let secret = serde_json::from_slice(&std::fs::read(&key)?)?;
    fixture.supervisor = Supervisor::launch_with_args(
        &socket,
        &fixture.journal,
        &key,
        &[
            "--test-output-reader-release".into(),
            release.as_os_str().to_owned(),
        ],
    )?;
    fixture.client = fixture.supervisor.connect(&socket, secret).await?;
    let result = async {
        let (_, terminal) = fixture.post_terminal(Some("real-reader-eof")).await?;
        tokio::time::timeout(Duration::from_secs(5), async {
            while !release.with_extension("ready").exists() || !release.with_extension("drained").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.context("actual reader/helper barriers were not reached")?;
        let json: String = sqlx::query_scalar("SELECT o.execution_request_json FROM operations o JOIN terminals t ON t.launch_operation_id=o.id WHERE t.id=?1")
            .bind(&terminal.id).fetch_one(fixture.sql.pool()).await?;
        let request: PrepareExecution = serde_json::from_value(serde_json::from_str::<Value>(&json)?["request"].clone())?;
        let mut client = observer(&fixture).await?;
        let ticket = calm_session::control::execution::ConsumeExecution {
            execution_id: request.identity.execution_id, ticket_id: request.identity.ticket_id, ticket: request.ticket,
        };
        let serialized = client.request(ExecutionRequest::Revoke(ticket.clone())).await?;
        ensure!(matches!(serialized, ExecutionReply::RevocationAcknowledged { state: calm_session::control::execution::ExecutionState::Exited { .. }, .. }),
            "the actual gated drain crossed the held reader before lifecycle serialization: {serialized:?}");
        let reply = client.request(ExecutionRequest::Query(ticket)).await?;
        ensure!(matches!(reply, ExecutionReply::Receipt { state: calm_session::control::execution::ExecutionState::Exited { .. }, .. }),
            "helper drain was published before the actual held reader reached EOF: {reply:?}");
        let exit: Option<i64> = sqlx::query_scalar("SELECT exit_code FROM terminals WHERE id=?1")
            .bind(&terminal.id).fetch_one(fixture.sql.pool()).await?;
        ensure!(exit.is_none(), "product persisted exit before final output was read");
        std::fs::write(&release, b"read actual PTY")?;
        let entry = fixture.state.terminal_renderer.get(&terminal.id).context("missing actual renderer")?;
        ensure!(entry.wait_exit_persisted_for_test(Duration::from_secs(5)).await, "final real output did not complete");
        ensure!(entry.handle.model_view.lock().unwrap().capture(0)?.0.text.iter().any(|line| line.contains("terminal-vertical-complete")),
            "durable drain lost the final product screen");
        Ok::<_, anyhow::Error>(())
    }.await;
    // Always release the real reader before orderly process cleanup.
    std::fs::write(&release, b"release for cleanup")?;
    fixture.stop().await;
    result?;
    ensure!(
        std::fs::read_to_string(&fixture.count)? == "executed\n",
        "EOF handling reissued the process"
    );
    Ok(())
}

#[tokio::test]
async fn terminal_exact_retired_result_replays_after_executor_restart() -> anyhow::Result<()> {
    use calm_session::control::execution::{EXECUTION_ENVELOPE_VERSION, ObjectRegistrationReceipt};
    let mut fixture = Fixture::start_in(Storage::Disk).await?;
    let result = async {
        let (_, terminal) = fixture.post_terminal(Some("retired-original")).await?;
        ensure!(fixture.state.terminal_renderer.get(&terminal.id).context("missing actual renderer")?
            .wait_exit_persisted_for_test(Duration::from_secs(5)).await, "original did not drain");
        let json: String = sqlx::query_scalar("SELECT o.execution_request_json FROM operations o JOIN terminals t ON t.launch_operation_id=o.id WHERE t.id=?1")
            .bind(&terminal.id).fetch_one(fixture.sql.pool()).await?;
        let transfer: AttachRegistration = serde_json::from_value(serde_json::from_str::<Value>(&json)?["registration"].clone())?;
        let expected = ObjectRegistrationReceipt { contract: EXECUTION_ENVELOPE_VERSION, id: transfer.registration_id.clone(), transfer };
        let mut client = observer(&fixture).await?;
        ensure!(matches!(client.request(ExecutionRequest::RetireRegistration(expected.clone())).await?,
            ExecutionReply::RegistrationRetired { registration } if registration == expected.id), "actual initial retirement failed");
        fixture.restart_executor().await?;
        let mut client = observer(&fixture).await?;
        let mut changed = expected.clone();
        changed.transfer.nonce = TransferNonce::fresh()?;
        ensure!(matches!(client.request(ExecutionRequest::RetireRegistration(changed)).await?, ExecutionReply::Refused { .. }),
            "restart accepted a changed immutable retirement input");
        let reply = client.request(ExecutionRequest::RetireRegistration(expected.clone())).await?;
        ensure!(matches!(&reply, ExecutionReply::RegistrationRetired { registration } if *registration == expected.id),
            "actual prior retirement did not replay across executor restart: {reply:?}");
        Ok::<_, anyhow::Error>(())
    }.await;
    fixture.stop().await;
    result?;
    ensure!(
        std::fs::read_to_string(&fixture.count)? == "executed\n",
        "retirement replay reissued the process"
    );
    Ok(())
}

#[tokio::test]
async fn terminal_delete_rollback_retains_screen_and_retries_release_after_restart()
-> anyhow::Result<()> {
    let mut fixture = Fixture::start_in(Storage::Disk).await?;
    let mut server = HttpServer::start(fixture.app.clone()).await?;
    let result = async {
        let cookie = login(&server).await?;
        let (status, _, card) = server.post(format!("/api/tracks/{}/terminal-cards", fixture.track), cookie.clone(), fixture.request_body()).await?;
        ensure!(status == 201, "original launch failed: {status} {card}");
        let terminal = fixture.sql.terminal_get_by_card(card["id"].as_str().context("missing card")?).await?.context("missing terminal")?;
        ensure!(fixture.state.terminal_renderer.get(&terminal.id).context("missing renderer")?
            .wait_exit_persisted_for_test(Duration::from_secs(5)).await, "original never completed");
        let original: String = sqlx::query_scalar("SELECT o.execution_request_json FROM operations o JOIN terminals t ON t.launch_operation_id=o.id WHERE t.id=?1")
            .bind(&terminal.id).fetch_one(fixture.sql.pool()).await?;
        // Inject only a real transaction failure; the launch/grant/native
        // request remain products of the actual authenticated HTTP path.
        sqlx::query("CREATE TRIGGER refuse_terminal_delete_for_test BEFORE DELETE ON terminals BEGIN SELECT RAISE(ABORT,'fixture rollback after quiescence'); END")
            .execute(fixture.sql.pool()).await?;
        let (status, _) = delete(&server, &cookie, format!("/api/cards/{}", terminal.card_id)).await?;
        ensure!(status == 500, "expected actual deletion transaction rollback: {status}");
        ensure!(fixture.sql.terminal_get(&terminal.id).await?.is_some(), "failed transaction removed the terminal");
        let entry = fixture.state.terminal_renderer.get(&terminal.id).context("deletion rollback lost the product renderer history")?;
        ensure!(entry.handle.model_view.lock().unwrap().capture(0)?.0.text.iter().any(|line| line.contains("terminal-vertical-complete")),
            "deletion rollback erased the original completed screen");
        drop(entry);
        server.stop().await?;
        fixture.rebuild_from_disk().await?;
        server = HttpServer::start(fixture.app.clone()).await?;
        let cookie = login(&server).await?;
        let (socket, snapshot) = open_with_snapshot(&server, &cookie, &terminal.id, 80, 24).await?;
        ensure!(String::from_utf8_lossy(&snapshot.data).contains("terminal-vertical-complete"),
            "cold product attach lost output after deletion rollback");
        disconnect(socket, fixture.state.terminal_renderer.get(&terminal.id).context("missing cold renderer")?).await?;
        let retained: String = sqlx::query_scalar("SELECT o.execution_request_json FROM operations o JOIN terminals t ON t.launch_operation_id=o.id WHERE t.id=?1")
            .bind(&terminal.id).fetch_one(fixture.sql.pool()).await?;
        ensure!(retained == original, "rollback retargeted the original request");
        sqlx::query("DROP TRIGGER refuse_terminal_delete_for_test").execute(fixture.sql.pool()).await?;
        fixture.restart_executor().await?;
        let (status, body) = delete(&server, &cookie, format!("/api/cards/{}", terminal.card_id)).await?;
        ensure!(status == 204, "retired original release did not replay through actual deletion after restart: {status} {body}");
        ensure!(fixture.sql.terminal_get(&terminal.id).await?.is_none(), "successful final deletion retained its row");
        Ok::<_, anyhow::Error>(())
    }.await;
    let server_result = server.stop().await;
    fixture.stop().await;
    result?;
    server_result?;
    ensure!(
        std::fs::read_to_string(&fixture.count)? == "executed\n",
        "deletion rollback/retry reissued the process"
    );
    Ok(())
}

#[tokio::test]
async fn terminal_delete_retains_rows_until_actual_registration_borrower_releases()
-> anyhow::Result<()> {
    use calm_session::control::execution::{
        EXECUTION_ENVELOPE_VERSION, ExecutionHandshake, ExecutionProtocolMode,
        ExportAcknowledgement, rights,
    };
    use calm_session::control::{ControlMsg, ControlReply};
    use calm_session::{read_frame, write_frame};
    let mut fixture = Fixture::start_in(Storage::Disk).await?;
    let mut server = HttpServer::start(fixture.app.clone()).await?;
    let result = async {
        let cookie = login(&server).await?;
        let (status, _, card) = server.post(format!("/api/tracks/{}/terminal-cards", fixture.track), cookie.clone(), fixture.request_body()).await?;
        ensure!(status == 201, "original create failed: {status} {card}");
        let terminal = fixture.sql.terminal_get_by_card(card["id"].as_str().context("missing card")?).await?.context("missing terminal")?;
        ensure!(fixture.state.terminal_renderer.get(&terminal.id).context("missing renderer")?
            .wait_exit_persisted_for_test(Duration::from_secs(5)).await, "original never completed");
        let json: String = sqlx::query_scalar("SELECT o.execution_request_json FROM operations o JOIN terminals t ON t.launch_operation_id=o.id WHERE t.id=?1")
            .bind(&terminal.id).fetch_one(fixture.sql.pool()).await?;
        let transfer: AttachRegistration = serde_json::from_value(serde_json::from_str::<Value>(&json)?["registration"].clone())?;
        let secret = serde_json::from_slice(&std::fs::read(fixture._root.path().join("kernel-key.json"))?)?;
        let mut stream = tokio::net::UnixStream::connect(fixture._sockets.path().join("control.sock")).await?;
        write_frame(&mut stream, &ControlMsg::Execution(Box::new(ExecutionRequest::Handshake(ExecutionHandshake {
            required_contract: EXECUTION_ENVELOPE_VERSION, client_secret: secret,
            mode: ExecutionProtocolMode::RegisteredV3,
        })))).await?;
        let _: ControlReply = read_frame(&mut stream).await?;
        let nonce = TransferNonce::fresh()?;
        rights::send(&stream, &ControlMsg::Execution(Box::new(ExecutionRequest::Export(ExportRegistration {
            executor_boot: transfer.executor_boot.clone(), registration: transfer.registration_id.clone(), nonce: nonce.clone(),
        }))), &[]).await?;
        let (reply, objects) = rights::receive::<ControlReply>(&stream).await?;
        ensure!(matches!(reply, ControlReply::Execution(reply) if matches!(&*reply, ExecutionReply::Exported { receipt, .. } if receipt.transfer == transfer)) && !objects.is_empty(),
            "actual reverse Export did not retain the original source lease");
        let (status, body) = delete(&server, &cookie, format!("/api/cards/{}", terminal.card_id)).await?;
        ensure!(status == 409 && body.contains("in-flight export"),
            "DELETE crossed the actual Retire refusal: {status} {body}");
        ensure!(fixture.sql.terminal_get(&terminal.id).await?.is_some(), "Retire refusal lost its Terminal owner");
        ensure!(fixture.state.terminal_renderer.get(&terminal.id).context("Retire refusal lost product history")?
            .handle.model_view.lock().unwrap().capture(0)?.0.text.iter().any(|line| line.contains("terminal-vertical-complete")),
            "Retire refusal discarded the completed screen");
        rights::send(&stream, &ControlMsg::Execution(Box::new(ExecutionRequest::ExportAck(ExportAcknowledgement {
            executor_boot: transfer.executor_boot.clone(), registration: transfer.registration_id.clone(), nonce,
        }))), &[]).await?;
        let (reply, _) = rights::receive::<ControlReply>(&stream).await?;
        ensure!(matches!(reply, ControlReply::Execution(reply) if matches!(*reply, ExecutionReply::ExportAcknowledged)), "actual export ACK failed");
        drop(objects); drop(stream);
        let (status, body) = delete(&server, &cookie, format!("/api/cards/{}", terminal.card_id)).await?;
        ensure!(status == 204, "exact final release did not retry: {status} {body}");
        Ok::<_, anyhow::Error>(())
    }.await;
    let server_result = server.stop().await;
    fixture.stop().await;
    result?;
    server_result?;
    ensure!(
        std::fs::read_to_string(&fixture.count)? == "executed\n",
        "release retry reissued the process"
    );
    Ok(())
}
