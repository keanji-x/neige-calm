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
