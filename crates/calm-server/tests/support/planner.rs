//! Production-shaped Planner fixtures shared across integration test binaries.
use calm_server::db::sqlite::session_start_runtime_tx;
use calm_server::db::{RepoEventWrite, write_in_tx_typed};
use calm_server::session_projection_repo::{AgentProvider, WorkerSessionInit, WorkerSessionState};
use serde_json::Value;

pub fn payload() -> Value {
    calm_server::track_creation::planner_harness_card_payload(
        &calm_server::acp_planner::config::AcpPlannerHost::unconfigured_scratch().unwrap(),
        None,
        AgentProvider::Codex,
    )
}

pub fn session_init(
    id: &str,
    card: &str,
    thread: Option<String>,
    state: WorkerSessionState,
    now: i64,
) -> WorkerSessionInit {
    WorkerSessionInit::shared_planner(
        id.into(),
        card.into(),
        AgentProvider::Codex,
        state,
        thread,
        serde_json::to_value(calm_server::harness::snapshot::HarnessSnapshot::initial(
            0,
            vec![],
        ))
        .unwrap(),
        now,
    )
}

pub async fn start_session(repo: &dyn RepoEventWrite, card: &str, session: &str) {
    let init = session_init(session, card, None, WorkerSessionState::Starting, 1);
    write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            session_start_runtime_tx(tx, init).await?;
            Ok(())
        })
    })
    .await
    .expect("start current Planner fixture");
}

/// Mint the card, current runtime and credentials together, like a managed Planner.
pub async fn mint_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    card: String,
    session: &str,
    track: calm_server::ids::TrackId,
    cache: &calm_server::card_role_cache::CardRoleCache,
) -> String {
    use calm_server::db::sqlite::{
        card_create_with_id_tx, card_mcp_token_set_tx, session_mcp_token_set_tx,
    };
    use calm_server::model::{CardRole, NewCard};
    card_create_with_id_tx(
        tx,
        card.clone(),
        NewCard {
            track_id: track,
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: payload(),
        },
        CardRole::Planner,
        false,
        cache,
    )
    .await
    .unwrap();
    session_start_runtime_tx(
        tx,
        session_init(
            session,
            &card,
            None,
            WorkerSessionState::Starting,
            calm_server::model::now_ms(),
        ),
    )
    .await
    .unwrap();
    let token = calm_server::mcp_server::auth::CardMcpToken::generate();
    let hash = calm_server::mcp_server::auth::hash_token(token.as_str());
    card_mcp_token_set_tx(tx, &card, &hash).await.unwrap();
    session_mcp_token_set_tx(tx, session, &hash).await.unwrap();
    token.into_inner()
}

/// Released REST receipt, allocated and delivered by the existing creation owner.
pub async fn historical_track(
    state: &calm_server::state::AppState,
    body: Value,
    key: Option<&str>,
) -> calm_server::error::Result<Value> {
    use axum::extract::FromRef;
    let track = calm_server::test_seams::historical_creator_receipt_for_test(
        &calm_server::state::RouteState::from_ref(state),
        100,
        calm_server::test_seams::HistoricalReceiptForTest::Released {
            request: serde_json::from_value(body)?,
            key: key.map(str::to_owned),
        },
    )
    .await?;
    Ok(serde_json::to_value(track)?)
}
