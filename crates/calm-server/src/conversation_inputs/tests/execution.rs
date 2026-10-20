//! The real auth ingress feeds the production claim facade. Public V1 routes
//! remain unchanged until the closed canonical FSM/reader/producer switch.
use super::*;
use crate::auth::HumanInputSource;
use crate::db::sqlite::conversation::{current_input_carrier_tx, submission_identity_tx};
use calm_types::conversation::{
    AcceptedInputRef, ConversationExecutionProvenance, FrozenHumanAttribution,
    SubmissionRenderInput, SubmissionRenderSegment,
};

async fn source(auth: &AuthState, token: &str) -> HumanInputSource {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::COOKIE,
        format!("{}={token}", crate::auth::SESSION_COOKIE)
            .parse()
            .unwrap(),
    );
    let (status, source) = crate::auth::authenticated_source(auth.clone(), headers).await;
    assert_eq!(status, StatusCode::OK);
    source.unwrap()
}

async fn first_plan(
    tx: &mut Transaction<'_, Sqlite>,
    carrier: &CurrentInputCarrier,
) -> SubmissionRenderPlan {
    let AcceptedInputRef::Human { message } = carrier.checkpoint().pending[0].input_ref() else {
        panic!("human fixture")
    };
    let identity = conversation::message_tx(tx, &message.message_id)
        .await
        .unwrap();
    let calm_types::conversation::MessageAuthor::AuthenticatedHuman { registered_user_id } =
        identity.author;
    let display_name: String =
        sqlx::query_scalar("SELECT display_name FROM application_users WHERE id=?1")
            .bind(&registered_user_id)
            .fetch_one(&mut **tx)
            .await
            .unwrap();
    SubmissionRenderPlan::V1 {
        segments: vec![SubmissionRenderSegment::Input {
            input_position: 0,
            input: SubmissionRenderInput::Human {
                message,
                author: FrozenHumanAttribution {
                    registered_user_id,
                    display_name,
                },
            },
        }],
    }
}

async fn claim(fx: &Fixture, source: &HumanInputSource) -> Result<ClaimedSubmission> {
    let mut tx = begin_immediate_tx(fx.input.repo.db()).await.unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &fx.input.card, &fx.input.runtime).await?;
    let plan = first_plan(&mut tx, &carrier).await;
    let result = claim_initial_tx(&mut tx, source, &mut carrier, plan).await;
    if result.is_ok() {
        tx.commit().await.unwrap();
    } else {
        tx.rollback().await.unwrap();
    }
    result
}

async fn accept_owner(fx: &Fixture) -> (String, HumanInputSource) {
    let token = fx.session(LEGACY_OWNER_USER_ID, "owner");
    assert_eq!(fx.send(Some(&token)).await.status(), StatusCode::OK);
    let source = source(&fx.auth, &token).await;
    (token, source)
}

async fn transfer_area_to_alice(fx: &Fixture) {
    fx.register("alice").await;
    let mut tx = begin_immediate_tx(fx.input.repo.db()).await.unwrap();
    let area = conversation::conversation_area_tx(&mut tx, &fx.input.card)
        .await
        .unwrap();
    sqlx::query("UPDATE area_members SET role='member' WHERE area_id=?1 AND role='owner'")
        .bind(area.id.as_str())
        .execute(&mut **tx)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO area_members(area_id,user_id,role,joined_at) VALUES(?1,'alice','owner',0)",
    )
    .bind(area.id.as_str())
    .execute(&mut **tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn operator_member_cannot_claim_initial_in_another_users_private_area() {
    let fx = Fixture::new(false).await;
    let (_, source) = accept_owner(&fx).await;
    transfer_area_to_alice(&fx).await;
    assert!(claim(&fx, &source).await.is_err());
}

#[tokio::test]
async fn human_source_cannot_authorize_a_machine_observation() {
    use calm_types::conversation::{
        PendingInput, PrivateHistoryPayload, SystemObservation, SystemObservationId,
    };
    let fx = Fixture::new(false).await;
    let token = fx.session(LEGACY_OWNER_USER_ID, "owner");
    let source = source(&fx.auth, &token).await;
    let observation =
        SystemObservation::try_from(calm_types::observation::Observation::SystemContext {
            text: "owned by its actual machine producer".into(),
        })
        .unwrap();
    let id = SystemObservationId::parse(&new_id()).unwrap();
    let checkpoint = ConversationInputCheckpoint {
        receipts: vec![],
        next_queue_position: 1,
        pending: vec![PendingInput::System {
            observation_id: id.clone(),
            observation: observation.clone(),
            queue_position: 0,
            queued_at_ms: 0,
            envelope_id: None,
            addressable: true,
        }],
        active: None,
        attempts: vec![],
    };
    let mut tx = begin_immediate_tx(fx.input.repo.db()).await.unwrap();
    let carrier = current_input_carrier_tx(&mut tx, &fx.input.card, &fx.input.runtime)
        .await
        .unwrap();
    let mut snapshot = carrier.snapshot().clone();
    snapshot["canonical_inputs"] = serde_json::to_value(checkpoint).unwrap();
    assert!(
        crate::db::sqlite::session_set_handle_state_tx(&mut tx, &fx.input.runtime, Some(snapshot))
            .await
            .unwrap()
    );
    let mut carrier = current_input_carrier_tx(&mut tx, &fx.input.card, &fx.input.runtime)
        .await
        .unwrap();
    let plan = SubmissionRenderPlan::V1 {
        segments: vec![SubmissionRenderSegment::Input {
            input_position: 0,
            input: SubmissionRenderInput::System {
                observation_id: id,
                payload: PrivateHistoryPayload::Present { value: observation },
            },
        }],
    };
    assert!(
        claim_initial_tx(&mut tx, &source, &mut carrier, plan)
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn real_owner_source_claims_initial_in_the_actual_scope() {
    let fx = Fixture::new(false).await;
    let (_, source) = accept_owner(&fx).await;
    let initial = claim(&fx, &source).await.unwrap();
    let mut tx = begin_immediate_tx(fx.input.repo.db()).await.unwrap();
    let identity = submission_identity_tx(&mut tx, &initial.submission_id)
        .await
        .unwrap();
    assert!(
        matches!(identity.execution, ConversationExecutionProvenance::LegacySingleUserDeployment {
        ref deployment_owner_user_id, ..
    } if deployment_owner_user_id == LEGACY_OWNER_USER_ID)
    );
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn actual_today_scope_accepts_owner_execution_without_area_membership() {
    let fx = Fixture::new(true).await;
    let (_, source) = accept_owner(&fx).await;
    assert!(claim(&fx, &source).await.is_ok());
}

#[tokio::test]
async fn actual_dev_ingress_can_claim_but_background_principal_cannot_supply_source() {
    let fx = Fixture::new(false).await;
    let _ = accept_owner(&fx).await;
    let mut auth = fx.auth.clone();
    let mut config = (*auth.config).clone();
    config.dev_autologin = true;
    auth.config = Arc::new(config);
    let (status, source) =
        crate::auth::authenticated_source(auth, axum::http::HeaderMap::new()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(claim(&fx, &source.unwrap()).await.is_ok());
}

#[tokio::test]
async fn current_session_revocation_holds_accepted_work_before_claim() {
    let fx = Fixture::new(false).await;
    let (token, source) = accept_owner(&fx).await;
    fx.auth.sessions.remove(&token).unwrap();
    assert!(claim(&fx, &source).await.is_err());
    let mut tx = begin_immediate_tx(fx.input.repo.db()).await.unwrap();
    let carrier = current_input_carrier_tx(&mut tx, &fx.input.card, &fx.input.runtime)
        .await
        .unwrap();
    assert_eq!(carrier.checkpoint().pending.len(), 1);
    assert!(carrier.checkpoint().active.is_none());
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn operator_source_cannot_claim_another_humans_input_even_in_owned_private_area() {
    let fx = Fixture::new(false).await;
    fx.register("alice").await;
    let mut tx = begin_immediate_tx(fx.input.repo.db()).await.unwrap();
    let area = conversation::conversation_area_tx(&mut tx, &fx.input.card)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO area_members(area_id,user_id,role,joined_at) VALUES(?1,'alice','member',0)",
    )
    .bind(area.id.as_str())
    .execute(&mut **tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let alice = fx.session("alice", "owner");
    assert_eq!(fx.send(Some(&alice)).await.status(), StatusCode::OK);
    let owner = fx.session(LEGACY_OWNER_USER_ID, "owner");
    assert!(claim(&fx, &source(&fx.auth, &owner).await).await.is_err());
    assert!(claim(&fx, &source(&fx.auth, &alice).await).await.is_err());
}
