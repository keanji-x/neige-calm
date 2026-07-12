//! Server-owned Today launchpad bootstrap (#951, Slice A).

use crate::actor::Actor;
use crate::db::sqlite::{
    card_create_with_id_tx, card_update_tx, card_with_terminal_create_tx, cove_create_system_tx,
};
use crate::db::{write_with_event_typed, write_with_events_typed};
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::{Event, EventScope};
use crate::ids::{ActorId, CardId, WaveId};
use crate::model::{
    Card, CardPatch, CardRole, NewCard, RequestTheme, Terminal, Wave, new_id, now_ms,
};
use crate::operation::spec_harness_start_adapter::SpecHarnessStartOperationPayload;
use crate::operation::{OperationKey, OperationOutcome};
use crate::routes::terminal_cards::stable_payload_hash;
use crate::state::{AppState, RouteState};
use crate::validation::CODEX_PAYLOAD_SCHEMA_VERSION;
use crate::wave_report::WaveReportPayload;
use axum::{
    Json, Router,
    extract::{FromRef, State},
    http::StatusCode,
    routing::post,
};
use serde::Serialize;
use sqlx::{Sqlite, Transaction};
use utoipa::ToSchema;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/today/launchpad/ensure", post(ensure_today_launchpad))
}

#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct TodayLaunchpad {
    pub wave_id: String,
    pub spec_card_id: String,
    pub terminal_card_id: String,
    pub terminal_id: String,
}

struct EnsureTxResult {
    dto: TodayLaunchpad,
    wave: Wave,
    report_card_id: String,
    created: bool,
}

fn spec_payload() -> serde_json::Value {
    serde_json::json!({
        "schemaVersion": CODEX_PAYLOAD_SCHEMA_VERSION,
        "harness": { "snapshotVersion": 0, "pendingQueue": [] }
    })
}

#[allow(deprecated)]
async fn today_launchpad_ensure_tx(
    tx: &mut Transaction<'_, Sqlite>,
    s: &RouteState,
    cove_id: &str,
    cwd: &str,
) -> Result<EnsureTxResult> {
    let existing = sqlx::query_as::<_, crate::db::rows::WaveRow>(
        "SELECT id,cove_id,title,sort,archived_at,pinned_at,lifecycle,cwd,workflow_id,purpose,workflow_input,terminal_at,created_at,updated_at FROM waves WHERE purpose='launchpad' LIMIT 1",
    ).fetch_optional(&mut **tx).await?.map(Wave::from);

    let (wave, created) = if let Some(wave) = existing {
        (wave, false)
    } else if let Some(mut wave) = sqlx::query_as::<_, crate::db::rows::WaveRow>(
        "SELECT id,cove_id,title,sort,archived_at,pinned_at,lifecycle,cwd,workflow_id,purpose,workflow_input,terminal_at,created_at,updated_at FROM waves WHERE cove_id=?1 AND purpose IS NULL AND title='Today' ORDER BY created_at,id LIMIT 1",
    ).bind(cove_id).fetch_optional(&mut **tx).await?.map(Wave::from) {
        sqlx::query("UPDATE waves SET purpose='launchpad', cwd=?2, workflow_id=NULL, workflow_input=NULL, updated_at=?3 WHERE id=?1")
            .bind(wave.id.as_str()).bind(cwd).bind(now_ms()).execute(&mut **tx).await?;
        wave.purpose = Some("launchpad".into()); wave.cwd = cwd.into();
        wave.workflow_id = None; wave.workflow_input = None;
        (wave, true)
    } else {
        let id = new_id(); let now = now_ms();
        let sort: f64 = sqlx::query_scalar("SELECT COALESCE(MAX(sort),-1)+1 FROM waves WHERE cove_id=?1")
            .bind(cove_id).fetch_one(&mut **tx).await?;
        sqlx::query("INSERT INTO waves(id,cove_id,title,sort,lifecycle,cwd,workflow_id,purpose,workflow_input,created_at,updated_at) VALUES(?1,?2,'Today',?3,'draft',?4,NULL,'launchpad',NULL,?5,?5)")
            .bind(&id).bind(cove_id).bind(sort).bind(cwd).bind(now).execute(&mut **tx).await?;
        s.write.cove_cache().insert(WaveId::from(id.clone()), cove_id.to_string().into());
        (Wave { id:id.into(), cove_id:cove_id.to_string().into(), title:"Today".into(), sort,
            archived_at:None, pinned_at:None, lifecycle:Default::default(), cwd:cwd.into(),
            workflow_id:None, purpose:Some("launchpad".into()), workflow_input:None,
            terminal_at:None, created_at:now, updated_at:now }, true)
    };

    let cards: Vec<Card> = sqlx::query_as::<_, crate::db::rows::CardRow>(
        "SELECT id,wave_id,kind,sort,payload,deletable,created_at,updated_at FROM cards WHERE wave_id=?1 ORDER BY created_at,id"
    ).bind(wave.id.as_str()).fetch_all(&mut **tx).await?.into_iter().map(Card::from).collect();
    let spec = if let Some(card) = cards
        .iter()
        .find(|c| c.kind == "codex" && s.write.role_cache().get(&c.id) == Some(CardRole::Spec))
        .cloned()
    {
        // Adoption deliberately resets only the spec transcript/thread surface.
        sqlx::query("DELETE FROM harness_items WHERE card_id=?1")
            .bind(card.id.as_str())
            .execute(&mut **tx)
            .await?;
        card_update_tx(
            tx,
            card.id.as_str(),
            CardPatch {
                payload: Some(spec_payload()),
                ..Default::default()
            },
        )
        .await?
    } else {
        card_create_with_id_tx(
            tx,
            new_id(),
            NewCard {
                wave_id: wave.id.clone(),
                kind: "codex".into(),
                sort: None,
                payload: spec_payload(),
            },
            CardRole::Spec,
            false,
            s.write.role_cache(),
        )
        .await?
    };
    let report = if let Some(card) = cards.iter().find(|c| c.kind == "wave-report").cloned() {
        card
    } else {
        card_create_with_id_tx(
            tx,
            new_id(),
            NewCard {
                wave_id: wave.id.clone(),
                kind: "wave-report".into(),
                sort: Some(-1.0),
                payload: serde_json::to_value(WaveReportPayload::initial())?,
            },
            CardRole::ReportCard,
            false,
            s.write.role_cache(),
        )
        .await?
    };
    let valid_terminal_card = sqlx::query_as::<_, crate::db::rows::CardRow>(
        "SELECT c.id,c.wave_id,c.kind,c.sort,c.payload,c.deletable,c.created_at,c.updated_at FROM cards c JOIN terminals t ON t.card_id=c.id WHERE c.wave_id=?1 AND c.kind='terminal' ORDER BY c.created_at,c.id LIMIT 1"
    ).bind(wave.id.as_str()).fetch_optional(&mut **tx).await?.map(Card::from);
    let valid_terminal: Option<(Card, Terminal)> = if let Some(card) = valid_terminal_card {
        let term = crate::db::sqlite::terminal_get_by_card_tx(tx, card.id.as_str()).await?;
        term.map(|term| (card, term))
    } else {
        None
    };
    let (terminal_card, terminal) = if let Some(pair) = valid_terminal {
        pair
    } else {
        card_with_terminal_create_tx(
            tx,
            new_id(),
            &new_id(),
            None,
            wave.id.clone(),
            None,
            String::new(),
            cwd.into(),
            serde_json::json!({}),
            CardRole::Worker,
            false,
            s.write.role_cache(),
            RequestTheme::default_dark(),
        )
        .await?
    };
    Ok(EnsureTxResult {
        dto: TodayLaunchpad {
            wave_id: wave.id.to_string(),
            spec_card_id: spec.id.to_string(),
            terminal_card_id: terminal_card.id.to_string(),
            terminal_id: terminal.id,
        },
        wave,
        report_card_id: report.id.to_string(),
        created,
    })
}

#[utoipa::path(post,path="/api/today/launchpad/ensure",tag="waves",responses(
    (status=200,description="Existing live launchpad",body=TodayLaunchpad),
    (status=201,description="Launchpad minted or adopted; harness start may still be dormant",body=TodayLaunchpad),
    (status=503,description="Launchpad exists but harness failed to start",body=ErrorBody)
))]
pub(crate) async fn ensure_today_launchpad(
    State(app): State<AppState>,
    _actor: Actor,
) -> Result<(StatusCode, Json<TodayLaunchpad>)> {
    let cove = if let Some(c) = app.repo.cove_get_system().await? {
        c
    } else {
        let route = RouteState::from_ref(&app);
        let minted = write_with_event_typed(
            app.repo.as_ref(),
            ActorId::Kernel,
            EventScope::System,
            None,
            &app.events,
            &route.write,
            |tx| {
                Box::pin(async move {
                    let c = cove_create_system_tx(tx).await?;
                    Ok((c.clone(), Event::CoveUpdated(c)))
                })
            },
        )
        .await;
        match minted {
            Ok((c, _)) => c,
            Err(CalmError::Db(_)) => app
                .repo
                .cove_get_system()
                .await?
                .ok_or_else(|| CalmError::Internal("system cove race had no winner".into()))?,
            Err(e) => return Err(e),
        }
    };
    let base = app.daemon.data_dir.parent().unwrap_or(&app.daemon.data_dir);
    let launchpad = base.join("launchpad");
    std::fs::create_dir_all(&launchpad)?;
    let launchpad = launchpad.canonicalize()?;
    if !launchpad.is_dir() {
        return Err(CalmError::Internal(
            "launchpad cwd is not a directory".into(),
        ));
    }
    let cwd = launchpad.to_string_lossy().into_owned();
    let route = RouteState::from_ref(&app);
    let write = route.write.clone();
    let cove_id = cove.id.to_string();
    let attempt = write_with_events_typed(
        app.repo.as_ref(),
        ActorId::Kernel,
        None,
        &app.events,
        &write,
        move |tx| {
            Box::pin(async move {
                let out = today_launchpad_ensure_tx(tx, &route, &cove_id, &cwd).await?;
                Ok((out, Vec::new()))
            })
        },
    )
    .await;
    let (out, _) = match attempt {
        Ok(v) => v,
        Err(e @ CalmError::Db(_)) => {
            // A concurrent inserter won the partial unique index; retry selects it.
            let route = RouteState::from_ref(&app);
            let write = route.write.clone();
            let cove_id = cove.id.to_string();
            let cwd = launchpad.to_string_lossy().into_owned();
            write_with_events_typed(
                app.repo.as_ref(),
                ActorId::Kernel,
                None,
                &app.events,
                &write,
                move |tx| {
                    Box::pin(async move {
                        let o = today_launchpad_ensure_tx(tx, &route, &cove_id, &cwd).await?;
                        Ok((o, Vec::new()))
                    })
                },
            )
            .await
            .map_err(|_| e)?
        }
        Err(e) => return Err(e),
    };
    let req = SpecHarnessStartOperationPayload {
        actor: ActorId::Kernel,
        wave_id: out.dto.wave_id.clone(),
        spec_card_id: CardId::from(out.dto.spec_card_id.clone()),
        report_card_id: Some(out.report_card_id),
        sort: None,
        cwd: out.wave.cwd.clone(),
        goal: None,
        reset_harness_items: out.created,
        force_new_thread: out.created,
    };
    let hash = stable_payload_hash(&serde_json::json!({"actor":"kernel","request":&req}))?;
    let op = app
        .operation_runtime
        .submit(
            "spec-harness-start",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(format!("today-launchpad:{}", out.dto.spec_card_id)),
                payload_hash: hash,
            },
            serde_json::to_value(req)?,
        )
        .await?;
    let result = app.operation_runtime.wait(&op).await?;
    match result.outcome {
        OperationOutcome::Succeeded { .. } | OperationOutcome::SucceededViaCollision { .. } => {
            Ok((
                if out.created {
                    StatusCode::CREATED
                } else {
                    StatusCode::OK
                },
                Json(out.dto),
            ))
        }
        _ => Err(CalmError::Internal(format!(
            "launchpad exists but harness start failed: {op}"
        ))),
    }
}
