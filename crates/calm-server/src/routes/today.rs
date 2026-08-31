//! Server-owned Today launchpad bootstrap (#951, Slice A).

use crate::actor::Actor;
use crate::db::rows::WAVE_SELECT_COLUMNS;
use crate::db::sqlite::{
    card_create_with_id_tx, card_update_tx, card_with_terminal_create_tx, cove_create_system_tx,
    wave_workspace_write_tx,
};
use crate::db::{write_in_tx_typed, write_with_event_typed};
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::{Event, EventScope};
use crate::ids::{ActorId, CardId, WaveId};
use crate::model::{
    Card, CardPatch, CardRole, NewCard, RequestTheme, Terminal, Wave, WaveWorkspace,
    WaveWorkspaceKind, new_id, now_ms,
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
    adopted_legacy: bool,
}

fn is_unique_constraint(error: &CalmError, constraint: &str) -> bool {
    let CalmError::Db(sqlx::Error::Database(error)) = error else {
        return false;
    };
    error.is_unique_violation() && error.message().contains(constraint)
}

/// #1147 S1 — the launchpad wave's workspace. `Attached`, and **never frozen**.
///
/// This is the one documented exception to "S1 mints every wave frozen", and
/// it exists because the launchpad wave is the one wave whose path this slice
/// can legally *re-point*: the adopt-legacy branch below takes an existing
/// `Today` wave and re-aims it at the caller's `cwd`, and `ensure` is
/// idempotent, so that branch runs against a row that has already been
/// through here.
///
/// Freezing it would make design D1's "`frozen_at` is one-shot and monotonic"
/// false on the very first slice: re-point + re-stamp is exactly the sequence
/// the latch forbids. The alternatives were worse — refusing to re-point
/// breaks `ensure`, and re-pointing while leaving a stale stamp makes the
/// stamp lie about which path was frozen.
///
/// The cost is that `attached + frozen_at IS NULL` becomes a reachable state.
/// It is bounded to the kernel-owned wave in the **system** cove
/// (`purpose='launchpad'`, minted under `cove_create_system_tx`), and
/// `only_system_cove_waves_may_be_unfrozen` in `tests/cases/today_launchpad.rs`
/// holds that bound. S3's PATCH must refuse system-cove waves outright, so no
/// user-reachable path ever sees an unfrozen workspace.
fn launchpad_workspace(cwd: &str) -> WaveWorkspace {
    WaveWorkspace {
        kind: WaveWorkspaceKind::Attached,
        path: cwd.to_string(),
        // Never `Some(..)`. See the doc comment: writing a stamp here is what
        // would break monotonicity on re-adoption.
        frozen_at: None,
    }
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
    let existing = sqlx::query_as::<_, crate::db::rows::WaveRow>(&format!(
        "SELECT {WAVE_SELECT_COLUMNS} FROM waves WHERE purpose='launchpad' LIMIT 1"
    ))
    .fetch_optional(&mut **tx)
    .await?
    .map(Wave::from);

    let (wave, created, adopted_legacy) = if let Some(wave) = existing {
        (wave, false, false)
    } else if let Some(mut wave) = sqlx::query_as::<_, crate::db::rows::WaveRow>(&format!(
        "SELECT {WAVE_SELECT_COLUMNS} FROM waves WHERE cove_id=?1 AND purpose IS NULL AND title='Today' ORDER BY created_at,id LIMIT 1"
    )).bind(cove_id).fetch_optional(&mut **tx).await?.map(Wave::from) {
        // #1147 S1 — this UPDATE used to carry `cwd=?2`, which made it a
        // second writer of a column that design D1 demotes to a projection of
        // `workspace.path`. It now writes everything *except* the workspace
        // and hands the workspace to the single writer below, in the same tx.
        sqlx::query("UPDATE waves SET purpose='launchpad', workflow_id=NULL, plugin_scope=NULL, workflow_input=NULL, updated_at=?2 WHERE id=?1")
            .bind(wave.id.as_str()).bind(now_ms()).execute(&mut **tx).await?;
        let workspace = launchpad_workspace(cwd);
        wave_workspace_write_tx(tx, wave.id.as_str(), &workspace).await?;
        wave.purpose = Some("launchpad".into()); wave.cwd_wire_alias = cwd.into(); wave.workspace = workspace;
        wave.workflow_id = None; wave.plugin_scope = None; wave.workflow_input = None;
        (wave, false, true)
    } else {
        let id = new_id(); let now = now_ms();
        let sort: f64 = sqlx::query_scalar("SELECT CAST(COALESCE(MAX(sort),-1)+1 AS REAL) FROM waves WHERE cove_id=?1")
            .bind(cove_id).fetch_one(&mut **tx).await?;
        // #1147 S1 — `cwd` is off this INSERT's column list (it falls to
        // migration 0018's `DEFAULT ''` for the remainder of this tx) and is
        // written together with the workspace columns by the single writer.
        sqlx::query("INSERT INTO waves(id,cove_id,title,sort,lifecycle,workflow_id,purpose,workflow_input,created_at,updated_at) VALUES(?1,?2,'Today',?3,'draft',NULL,'launchpad',NULL,?4,?4)")
            .bind(&id).bind(cove_id).bind(sort).bind(now).execute(&mut **tx).await?;
        let workspace = launchpad_workspace(cwd);
        wave_workspace_write_tx(tx, &id, &workspace).await?;
        s.write.cove_cache().insert(WaveId::from(id.clone()), cove_id.to_string().into());
        (Wave { id:id.into(), cove_id:cove_id.to_string().into(), title:"Today".into(), sort,
            archived_at:None, pinned_at:None, lifecycle:Default::default(), cwd_wire_alias:cwd.into(),
            workflow_id:None, plugin_scope:None, purpose:Some("launchpad".into()), workflow_input:None,
            terminal_at:None, workspace, created_at:now, updated_at:now }, true, false)
    };

    let cards: Vec<Card> = sqlx::query_as::<_, crate::db::rows::CardRow>(
        "SELECT id,wave_id,kind,title,sort,payload,deletable,created_at,updated_at FROM cards WHERE wave_id=?1 ORDER BY created_at,id"
    ).bind(wave.id.as_str()).fetch_all(&mut **tx).await?.into_iter().map(Card::from).collect();
    let spec = if let Some(card) = cards
        .iter()
        .find(|c| c.kind == "codex" && s.write.role_cache().get(&c.id) == Some(CardRole::Spec))
        .cloned()
    {
        if adopted_legacy {
            // Only repurposing a legacy Today wave invalidates its old spec thread.
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
            card
        }
    } else {
        card_create_with_id_tx(
            tx,
            new_id(),
            NewCard {
                title: None,
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
                title: None,
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
        "SELECT c.id,c.wave_id,c.kind,c.title,c.sort,c.payload,c.deletable,c.created_at,c.updated_at FROM cards c JOIN terminals t ON t.card_id=c.id WHERE c.wave_id=?1 AND c.kind='terminal' ORDER BY c.created_at,c.id LIMIT 1"
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
        adopted_legacy,
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
            Err(e) if is_unique_constraint(&e, "idx_coves_one_system") => app
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
    let cove_id = cove.id.to_string();
    let attempt = write_in_tx_typed(app.repo.as_ref(), move |tx| {
        Box::pin(async move { today_launchpad_ensure_tx(tx, &route, &cove_id, &cwd).await })
    })
    .await;
    let out = match attempt {
        Ok(v) => v,
        Err(e) if is_unique_constraint(&e, "idx_waves_one_launchpad") => {
            // A concurrent inserter won the partial unique index; retry selects it.
            let route = RouteState::from_ref(&app);
            let cove_id = cove.id.to_string();
            let cwd = launchpad.to_string_lossy().into_owned();
            write_in_tx_typed(app.repo.as_ref(), move |tx| {
                Box::pin(async move { today_launchpad_ensure_tx(tx, &route, &cove_id, &cwd).await })
            })
            .await?
        }
        Err(e) => return Err(e),
    };
    // #1147 S2 (design D3) — the launchpad is the fifth wave-create entry
    // point and it does **not** go through `create_wave_structure`, so it
    // carries its own materialize call. Skipping it would leave every codex
    // task on the Today panel dying with `spawn-failed`
    // (`git rev-parse --show-toplevel` on a non-repository), which is the
    // exact defect #1147 opened on.
    //
    // Two deliberate departures from the managed path, both from design D9:
    //   * the workspace stays `Attached` at the kernel-owned
    //     `<data_dir>/../launchpad` directory rather than moving under the
    //     workspace root — re-pointing a live wave's cwd is a data migration
    //     this slice does not need, and existing installs (whose launchpad row
    //     already exists and is returned unchanged by the first branch above)
    //     would otherwise never be materialized at all;
    //   * it therefore materializes an `Attached` path, which no other caller
    //     may do. The rule "attached is never created or `git init`-ed" exists
    //     to protect *user* repositories; this directory is minted by
    //     `ensure_today_launchpad` a few lines up and is owned by the kernel.
    crate::workspace_materialize::materialize_managed_workspace(std::path::Path::new(
        &out.wave.workspace.path,
    ))
    .map_err(|error| {
        tracing::error!(
            wave_id = %out.dto.wave_id,
            path = %out.wave.workspace.path,
            error = %error,
            "today launchpad: workspace materialization failed"
        );
        error
    })?;

    let req = SpecHarnessStartOperationPayload {
        actor: ActorId::Kernel,
        wave_id: out.dto.wave_id.clone(),
        spec_card_id: CardId::from(out.dto.spec_card_id.clone()),
        report_card_id: Some(out.report_card_id),
        sort: None,
        cwd: out.wave.workspace.path.clone(),
        goal: None,
        reset_harness_items: out.created || out.adopted_legacy,
        force_new_thread: out.created || out.adopted_legacy,
        profile: Default::default(),
        create_card: None,
        first_message_sha256: None,
    };
    let start_mode = if out.created || out.adopted_legacy {
        "bootstrap"
    } else {
        "reuse"
    };
    let hash = stable_payload_hash(&serde_json::json!({"actor":"kernel","request":&req}))?;
    let op = app
        .operation_runtime
        .submit(
            "spec-harness-start",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(format!(
                    "today-launchpad:{}:{start_mode}",
                    out.dto.spec_card_id
                )),
                payload_hash: hash,
            },
            serde_json::to_value(req)?,
        )
        .await?;
    let result = app.operation_runtime.wait(&op).await?;
    match result.outcome {
        OperationOutcome::Succeeded { .. } | OperationOutcome::SucceededViaCollision { .. } => {
            Ok((
                if out.created || out.adopted_legacy {
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
