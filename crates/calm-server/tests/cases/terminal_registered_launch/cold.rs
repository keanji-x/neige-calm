//! Disk-backed recovery through the product's authenticated lazy WS attach.
use super::{ws::*, *};
use calm_session::{ClientMsg, DaemonMsg};

async fn original_request(fixture: &Fixture, terminal: &str) -> anyhow::Result<String> {
    Ok(sqlx::query_scalar("SELECT o.execution_request_json FROM operations o JOIN terminals t ON t.launch_operation_id=o.id WHERE t.id=?1")
        .bind(terminal).fetch_one(fixture.sql.pool()).await?)
}

async fn exercise(fixture: &mut Fixture, server: &mut HttpServer) -> anyhow::Result<()> {
    install_interactive_program(fixture)?;
    let cookie = login(server).await?;
    let (status, _, card) = server
        .post(
            format!("/api/tracks/{}/terminal-cards", fixture.track),
            cookie.clone(),
            fixture.request_body(),
        )
        .await?;
    ensure!(
        status == 201,
        "real HTTP Terminal launch failed: {status} {card}"
    );
    let terminal = fixture
        .sql
        .terminal_get_by_card(card["id"].as_str().context("missing card id")?)
        .await?
        .context("missing Terminal")?;
    let original = original_request(fixture, &terminal.id).await?;
    let mut socket = open(server, &cookie, &terminal.id, 80, 24).await?;
    send(
        &mut socket,
        ClientMsg::ResizeCommit {
            epoch: 1,
            cols: 113,
            rows: 37,
        },
    )
    .await?;
    until(&mut socket, |message| {
        matches!(
            message,
            DaemonMsg::ResizeApplied {
                epoch: 1,
                cols: 113,
                rows: 37,
                ..
            }
        )
    })
    .await?;
    input(&mut socket, 1, "size", b"37 113").await?;
    disconnect(
        socket,
        fixture
            .state
            .terminal_renderer
            .get(&terminal.id)
            .context("missing original renderer during disconnect")?,
    )
    .await?;
    // Plain browser reconnect also stays on the exact already-running child.
    let mut socket = open(server, &cookie, &terminal.id, 113, 37).await?;
    input(
        &mut socket,
        1,
        "browser-reconnect",
        b"received:browser-reconnect",
    )
    .await?;
    disconnect(
        socket,
        fixture
            .state
            .terminal_renderer
            .get(&terminal.id)
            .context("missing original renderer during disconnect")?,
    )
    .await?;
    server.stop().await?;
    fixture.rebuild_from_disk().await?;
    ensure!(
        fixture.state.terminal_renderer.get(&terminal.id).is_none(),
        "new AppState inherited a renderer cache"
    );
    *server = HttpServer::start(fixture.app.clone()).await?;
    // The new actual auth issuer has no session for the old cookie.
    let rejection = open(server, &cookie, &terminal.id, 113, 37)
        .await
        .err()
        .context("cold auth issuer accepted the old session cookie")?;
    ensure!(
        matches!(rejection.downcast_ref::<tokio_tungstenite::tungstenite::Error>(),
        Some(tokio_tungstenite::tungstenite::Error::Http(response)) if response.status().as_u16()==401),
        "old cookie was not rejected by actual authentication: {rejection:#}"
    );
    let new_cookie = login(server).await?;
    let mut socket = open(server, &new_cookie, &terminal.id, 113, 37).await?;
    ensure!(
        !fixture
            .state
            .terminal_renderer
            .get(&terminal.id)
            .context("product lazy attach installed no renderer")?
            .observable(),
        "recovered output bytes invented the missing PTY geometry history"
    );
    input(&mut socket, 1, "cold-reconnect", b"received:cold-reconnect").await?;
    input(&mut socket, 2, "size", b"37 113").await?;
    ensure!(
        original_request(fixture, &terminal.id).await? == original,
        "cold restoration changed the immutable original launch request"
    );
    send(&mut socket, ClientMsg::Kill).await?;
    until(&mut socket, |message| {
        matches!(message, DaemonMsg::TerminalExited { .. })
    })
    .await?;
    let _ = socket.close(None).await;
    Ok(())
}

#[tokio::test]
async fn terminal_disk_appstate_rebuild_reauthenticates_and_restores_original_resized_execution()
-> anyhow::Result<()> {
    let mut fixture = Fixture::start_in(Storage::Disk).await?;
    let mut server = HttpServer::start(fixture.app.clone()).await?;
    let result = exercise(&mut fixture, &mut server).await;
    let server_result = server.stop().await;
    fixture.stop().await;
    result?;
    server_result?;
    ensure!(
        std::fs::read_to_string(&fixture.count)? == "executed\n",
        "browser reconnect or cold AppState restoration reforked the original Terminal"
    );
    Ok(())
}
