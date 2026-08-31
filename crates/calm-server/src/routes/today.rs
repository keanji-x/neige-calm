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
use std::path::Path;
use utoipa::ToSchema;
// #1147 — one definition of the path digest, shared with the scheduler's
// child-wave bootstrap key.
use crate::workspace_materialize::workspace_key_digest;

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
    /// #1147 — the spec harness has never successfully started at the
    /// launchpad's *current* workspace path, so its thread must be re-opened.
    ///
    /// True on a fresh mint, on the one-time migration of a pre-S2 row, and on
    /// every retry after a failure in between — because it is derived from
    /// `operations` rather than from a one-shot in-memory comparison (N3).
    /// Without that, a materialize failure or a crash between commit and
    /// operation-submit would silently strand the agent in the old directory.
    repointed: bool,
}

fn is_unique_constraint(error: &CalmError, constraint: &str) -> bool {
    let CalmError::Db(sqlx::Error::Database(error)) = error else {
        return false;
    };
    error.is_unique_violation() && error.message().contains(constraint)
}

/// #1147 — the launchpad wave's workspace. `Managed`, under the workspace
/// root like every other managed workspace, and **never frozen**.
///
/// **Never frozen** is the design D9 exception, and it is the *only* thing
/// that is exceptional here. The launchpad is the one wave whose path the
/// kernel may legally re-point: the adopt-legacy branch below repurposes an
/// existing `Today` wave, and `ensure` is idempotent, so that branch runs
/// against a row that has already been through here. Freezing it would make
/// design D1's "`frozen_at` is one-shot and monotonic" false — re-point +
/// re-stamp is exactly the sequence the latch forbids. The alternatives were
/// worse: refusing to re-point breaks `ensure`, and re-pointing while leaving
/// a stale stamp makes the stamp lie about which path was frozen.
///
/// **`Managed`, not `Attached` (S2 review ruling).** S1 wrote `Attached` here
/// because managed roots did not exist yet, and S2's first cut kept it,
/// materializing the kernel-minted `<data_dir>/../launchpad` directory in
/// place. That made the row's label disagree with the fact on disk:
/// `Attached` means "a repository the *user* pointed at, never created or
/// `git init`-ed by the server", and this directory is created by the server.
/// A label that disagrees with the fact is the class of defect the previous
/// two slices spent three review rounds removing.
///
/// Making it managed also buys an invariant with **no exceptions**:
/// `kind = Managed ⇒ path is under <workspace-root>`.
/// `every_managed_wave_lives_under_the_workspace_root` in
/// `tests/cases/today_launchpad.rs` asserts it over the whole table, so S5's
/// recycle-path prefix assertion needs no launchpad carve-out.
///
/// The old `<data_dir>/../launchpad` directory is deliberately **left on
/// disk**: nothing outside the workspace root is ours to delete.
fn launchpad_workspace(workspace_root: &Path, cove_id: &str, wave_id: &str) -> WaveWorkspace {
    WaveWorkspace {
        kind: WaveWorkspaceKind::Managed,
        path: crate::workspace_materialize::managed_workspace_path(
            workspace_root,
            cove_id,
            wave_id,
        )
        .to_string_lossy()
        .into_owned(),
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
    workspace_root: &Path,
) -> Result<EnsureTxResult> {
    let existing = sqlx::query_as::<_, crate::db::rows::WaveRow>(&format!(
        "SELECT {WAVE_SELECT_COLUMNS} FROM waves WHERE purpose='launchpad' LIMIT 1"
    ))
    .fetch_optional(&mut **tx)
    .await?
    .map(Wave::from);

    let (mut wave, created, adopted_legacy) = if let Some(wave) = existing {
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
        wave.purpose = Some("launchpad".into());
        wave.workflow_id = None; wave.plugin_scope = None; wave.workflow_input = None;
        (wave, false, true)
    } else {
        let id = new_id(); let now = now_ms();
        let sort: f64 = sqlx::query_scalar("SELECT CAST(COALESCE(MAX(sort),-1)+1 AS REAL) FROM waves WHERE cove_id=?1")
            .bind(cove_id).fetch_one(&mut **tx).await?;
        // #1147 S1 — `cwd` is off this INSERT's column list (it falls to
        // migration 0018's `DEFAULT ''` for the remainder of this tx) and is
        // written together with the workspace columns by the single workspace
        // writer below, shared by all three branches.
        sqlx::query("INSERT INTO waves(id,cove_id,title,sort,lifecycle,workflow_id,purpose,workflow_input,created_at,updated_at) VALUES(?1,?2,'Today',?3,'draft',NULL,'launchpad',NULL,?4,?4)")
            .bind(&id).bind(cove_id).bind(sort).bind(now).execute(&mut **tx).await?;
        s.write.cove_cache().insert(WaveId::from(id.clone()), cove_id.to_string().into());
        (Wave { id:id.into(), cove_id:cove_id.to_string().into(), title:"Today".into(), sort,
            archived_at:None, pinned_at:None, lifecycle:Default::default(), cwd_wire_alias:String::new(),
            workflow_id:None, plugin_scope:None, purpose:Some("launchpad".into()), workflow_input:None,
            terminal_at:None, workspace: WaveWorkspace::default(), created_at:now, updated_at:now }, true, false)
    };

    // #1147 — ONE workspace writer for all three branches, so the launchpad's
    // row cannot differ by which branch minted it. The desired workspace is a
    // pure function of the wave id, so this is a no-op on the steady state and
    // a one-time re-point for a row created before S2 (whose path was the
    // kernel-minted `<data_dir>/../launchpad`). Re-pointing is legal precisely
    // because this wave is never frozen — see `launchpad_workspace`.
    let desired = launchpad_workspace(workspace_root, cove_id, wave.id.as_str());
    if wave.workspace != desired {
        wave_workspace_write_tx(tx, wave.id.as_str(), &desired).await?;
        wave.cwd_wire_alias = desired.path.clone();
        wave.workspace = desired;
    }
    let cwd = wave.workspace.path.clone();
    let cwd = cwd.as_str();

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
    // #1147 N3 — "does the spec harness need re-anchoring?" must be derived
    // from DURABLE state, not from the in-memory comparison above.
    //
    // That comparison is true for exactly one `ensure`: the one whose
    // transaction moves the path. Materialization runs after that transaction
    // commits, so if it fails (500), or the process dies before the
    // `spec-harness-start` operation is recorded, the intent is gone. The next
    // `ensure` sees `stored == desired`, concludes "steady state", and starts
    // the harness with `force_new_thread: false` — leaving the spec agent's
    // codex thread pinned to the OLD cwd forever while every worker uses the
    // new one. Reproduced end to end by the reviewer.
    //
    // The durable question is instead: *has a harness start ever succeeded at
    // THIS path?* The path digest is already in the idempotency key, so the
    // `operations` table answers it directly, and the answer survives every
    // crash window because it is only written once the start actually
    // succeeded.
    let started_at_this_path: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM operations \
         WHERE kind='spec-harness-start' AND phase='succeeded' AND idempotency_key LIKE ?1)",
    )
    .bind(format!(
        "today-launchpad:{}:%:{}",
        spec.id.as_str(),
        workspace_key_digest(&wave.workspace.path)
    ))
    .fetch_one(&mut **tx)
    .await?;
    let repointed = !started_at_this_path;

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
        repointed,
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
    // #1147 — the launchpad's workspace is a managed one under the workspace
    // root, derived from the wave id inside the transaction. The pre-S2
    // `<data_dir>/../launchpad` directory is no longer created here, and an
    // existing one is deliberately left on disk: nothing outside the workspace
    // root is ours to remove.
    let workspace_root = app.workspace_root().to_path_buf();
    let route = RouteState::from_ref(&app);
    let cove_id = cove.id.to_string();
    let root_for_tx = workspace_root.clone();
    let attempt = write_in_tx_typed(app.repo.as_ref(), move |tx| {
        Box::pin(async move { today_launchpad_ensure_tx(tx, &route, &cove_id, &root_for_tx).await })
    })
    .await;
    let out = match attempt {
        Ok(v) => v,
        Err(e) if is_unique_constraint(&e, "idx_waves_one_launchpad") => {
            // A concurrent inserter won the partial unique index; retry selects it.
            let route = RouteState::from_ref(&app);
            let cove_id = cove.id.to_string();
            let root_for_tx = workspace_root.clone();
            write_in_tx_typed(app.repo.as_ref(), move |tx| {
                Box::pin(async move {
                    today_launchpad_ensure_tx(tx, &route, &cove_id, &root_for_tx).await
                })
            })
            .await?
        }
        Err(e) => return Err(e),
    };
    // #1147 S2 (design D3) — the launchpad is the fifth wave-create entry
    // point and it does **not** go through `create_wave_structure` (raw
    // `INSERT INTO waves`), so it carries its own materialize call. Skipping it
    // would leave every codex task on the Today panel dying with
    // `spawn-failed` (`git rev-parse --show-toplevel` on a non-repository),
    // which is the exact defect #1147 opened on.
    crate::workspace_materialize::materialize_workspace(
        &out.wave.workspace,
        &workspace_root,
        out.wave.id.as_str(),
    )
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
        // #1147 — a re-point also forces a new thread. The codex thread holds
        // the cwd it was minted with, so resuming it after the workspace moved
        // would leave the spec agent working in the old directory while every
        // worker uses the new one. The transcript is NOT reset: harness items
        // are persisted per card, not per thread (`db/sqlite/read.rs`,
        // `WHERE card_id = ?1`), so re-opening the thread costs the agent its
        // in-thread context, not the user's history.
        force_new_thread: out.created || out.adopted_legacy || out.repointed,
        profile: Default::default(),
        create_card: None,
        first_message_sha256: None,
    };
    let start_mode = if out.created || out.adopted_legacy {
        "bootstrap"
    } else if out.repointed {
        // A distinct mode so the re-point's operation is not collapsed onto a
        // previously succeeded `reuse` by the idempotency key.
        "repoint"
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
                // #1147 S2 (red-team B1) — the workspace path is part of the
                // key, not just of the payload.
                //
                // The operation runtime refuses an idempotency key that was
                // already used with a *different* payload hash, and the
                // payload carries `cwd`. A pre-S2 database already holds
                // `today-launchpad:<card>:reuse` rows hashed against the old
                // `<data_dir>/../launchpad` path; after the upgrade re-points
                // the workspace, every subsequent `ensure` would submit that
                // same key with a new cwd and be rejected — 409, on every
                // request, forever, because nothing ever deletes rows from
                // `operations`. The Today panel would be dead from the first
                // request after deploy. The same mechanism fires on any
                // `CALM_WORKSPACE_ROOT` change.
                //
                // Keying on the path makes a re-point mint a *new* key instead
                // of colliding with the old one, and keeps idempotency exactly
                // as strong within one workspace.
                idempotency_key: Some(format!(
                    "today-launchpad:{}:{start_mode}:{}",
                    out.dto.spec_card_id,
                    workspace_key_digest(&out.wave.workspace.path)
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
