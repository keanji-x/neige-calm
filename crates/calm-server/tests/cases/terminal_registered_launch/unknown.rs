//! A real executor reboot cannot turn an occupied source into permission to fork.
use super::{ws::*, *};
use calm_session::control::execution::{ConsumeExecution, ExecutionState};

async fn exercise(
    fixture: &mut Fixture,
    server: &HttpServer,
    attempted_body: Value,
) -> anyhow::Result<()> {
    install_interactive_program(fixture)?;
    let cookie = login(server).await?;
    let key = "terminal-executor-restart-original";
    let (status, _, card) = post_at(
        server.address,
        format!("/api/tracks/{}/terminal-cards", fixture.track),
        cookie.clone(),
        fixture.request_body(),
        Some(key.into()),
    )
    .await?;
    ensure!(
        status == 201,
        "original HTTP Terminal failed: {status} {card}"
    );
    let terminal = fixture
        .sql
        .terminal_get_by_card(card["id"].as_str().context("missing card id")?)
        .await?
        .context("missing Terminal")?;
    let original:String=sqlx::query_scalar("SELECT o.execution_request_json FROM operations o JOIN terminals t ON t.launch_operation_id=o.id WHERE t.id=?1")
        .bind(&terminal.id).fetch_one(fixture.sql.pool()).await?;
    let request: PrepareExecution =
        serde_json::from_value(serde_json::from_str::<Value>(&original)?["request"].clone())?;
    let mut socket = open(server, &cookie, &terminal.id, 80, 24).await?;
    input(
        &mut socket,
        1,
        "before-executor-crash",
        b"received:before-executor-crash",
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
    fixture.restart_executor().await?;
    let secret = serde_json::from_slice(&std::fs::read(
        fixture._root.path().join("kernel-key.json"),
    )?)?;
    let mut observer = RegisteredExecutorConnection::connect_with_query(
        fixture._sockets.path().join("control.sock"),
        secret,
    )
    .await?;
    ensure!(
        observer.executor_boot() != &request.identity.executor_boot,
        "executor did not actually restart"
    );
    let query = ConsumeExecution {
        execution_id: request.identity.execution_id.clone(),
        ticket_id: request.identity.ticket_id.clone(),
        ticket: request.ticket.clone(),
    };
    ensure!(
        matches!(
            observer
                .request(ExecutionRequest::Query(query.clone()))
                .await?,
            ExecutionReply::Receipt {
                state: ExecutionState::Unknown { .. },
                ..
            }
        ),
        "executor restart invented settlement for the original live ticket"
    );
    for key in ["blocked-new-source-one", "blocked-new-source-two"] {
        let (status, _, body) = post_at(
            server.address,
            format!("/api/tracks/{}/terminal-cards", fixture.track),
            cookie.clone(),
            attempted_body.clone(),
            Some(key.into()),
        )
        .await?;
        ensure!(
            status != 201,
            "old Unknown execution allowed a new completed launch: {body}"
        );
        // A rejected new source has never reserved a per-operation request.
        // Compensation can remove that kernel-owned intent without making an
        // absence claim about the original Unknown execution.
        let refused: Option<(Option<String>, String)> = sqlx::query_as(
            "SELECT execution_request_json,phase FROM operations WHERE kind='terminal-create' AND idempotency_key=?1",
        ).bind(key).fetch_optional(fixture.sql.pool()).await?;
        if attempted_body.get("cwd").is_some() {
            ensure!(
                refused.is_none(),
                "explicit registered cwd reached operation insertion"
            );
        } else {
            ensure!(
                matches!(refused, Some((None, ref phase)) if phase == "failed"),
                "refused source retained an issued request or failed compensation: {refused:?} {body}"
            );
        }
        let terminals: i64 = sqlx::query_scalar("SELECT count(*) FROM terminals")
            .fetch_one(fixture.sql.pool())
            .await?;
        ensure!(
            terminals == 1,
            "refused never-issued launch leaked its Terminal row"
        );
    }
    // Original result replay needs its authenticated accessor, not a new source
    // lease or permission to replace the execution under another executor boot.
    let (status, _, replay) = post_at(
        server.address,
        format!("/api/tracks/{}/terminal-cards", fixture.track),
        cookie,
        fixture.request_body(),
        Some(key.into()),
    )
    .await?;
    ensure!(
        status == 201 && replay["id"] == card["id"],
        "same-key original result became a reissue or required current source: {status} {replay}"
    );
    let retained:String=sqlx::query_scalar("SELECT o.execution_request_json FROM operations o JOIN terminals t ON t.launch_operation_id=o.id WHERE t.id=?1")
        .bind(&terminal.id).fetch_one(fixture.sql.pool()).await?;
    ensure!(
        retained == original,
        "Unknown recovery retargeted the immutable original request"
    );
    ensure!(
        matches!(
            observer.request(ExecutionRequest::Query(query)).await?,
            ExecutionReply::Receipt {
                state: ExecutionState::Unknown { .. },
                ..
            }
        ),
        "a refused replacement changed the original Unknown into settlement"
    );
    let executions = std::fs::read_dir(&fixture.journal)?
        .filter(|entry| {
            entry.as_ref().is_ok_and(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "execution")
            })
        })
        .count();
    ensure!(
        executions == 1,
        "Unknown source admitted another durable execution: {executions}"
    );
    Ok(())
}

#[tokio::test]
async fn terminal_executor_restart_unknown_blocks_new_source_without_reissuing_original()
-> anyhow::Result<()> {
    let mut fixture = Fixture::start_in(Storage::Disk).await?;
    let mut server = HttpServer::start(fixture.app.clone()).await?;
    let body = fixture.request_body();
    let result = exercise(&mut fixture, &server, body).await;
    let server_result = server.stop().await;
    fixture.stop().await;
    result?;
    server_result?;
    ensure!(
        std::fs::read_to_string(&fixture.count)? == "executed\n",
        "executor Unknown or original ACK replay duplicated the process"
    );
    Ok(())
}

#[tokio::test]
async fn terminal_registered_explicit_cwd_cannot_bypass_unknown_selected_source()
-> anyhow::Result<()> {
    let mut fixture = Fixture::start_in(Storage::Disk).await?;
    let mut server = HttpServer::start(fixture.app.clone()).await?;
    // One wire-field difference from the existing Unknown sibling: the user
    // explicitly names the same directory that the real owner selected.
    let mut body = fixture.request_body();
    body["cwd"] = json!(fixture.cwd);
    let result = exercise(&mut fixture, &server, body).await;
    let server_result = server.stop().await;
    fixture.stop().await;
    result?;
    server_result?;
    ensure!(
        std::fs::read_to_string(&fixture.count)? == "executed\n",
        "explicit cwd bypass issued another real process"
    );
    Ok(())
}
