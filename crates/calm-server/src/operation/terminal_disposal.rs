//! Destruction must preserve an unresolved prepared launch, even when a probe currently finds no process.
use super::{Phase, Tx, terminal_launch::RequestState};
use crate::db::{RouteRepo, write_in_tx_typed};
use crate::error::{CalmError, Result};
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub(crate) enum Scope {
    Card(String),
    Terminal(String),
    Track(String),
    Area(String),
}
struct Unresolved {
    operation_id: String,
    terminal_id: String,
    socket: Option<PathBuf>,
}
impl Unresolved {
    fn error(&self) -> CalmError {
        CalmError::Conflict(format!(
            "operation {} has an unresolved launch for terminal {}; rows and workspace retained for reconciliation before deletion or relocation",
            self.operation_id, self.terminal_id
        ))
    }
}

fn unresolved_query(scope: &Scope) -> (String, &str) {
    let (column, id) = match scope {
        Scope::Card(id) => ("c.id", id),
        Scope::Terminal(id) => ("json_extract(o.tx_output_json,'$.data.terminal_id')", id),
        Scope::Track(id) => ("c.track_id", id),
        Scope::Area(id) => ("t.area_id", id),
    };
    // Column is selected exclusively from the static scope above.
    let sql = format!(
        "SELECT o.* FROM operations o JOIN cards c ON o.target_type='card' AND o.target_id=c.id JOIN tracks t ON t.id=c.track_id WHERE o.kind IN ('terminal-worker','claude-worker','codex-worker') AND {column}=?1 ORDER BY o.id"
    );
    (sql, id)
}

async fn unresolved_tx(tx: &mut Tx<'_>, scope: &Scope) -> Result<Vec<Unresolved>> {
    let (sql, id) = unresolved_query(scope);
    let rows = sqlx::query(&sql).bind(id).fetch_all(&mut **tx).await?;
    let mut unresolved = Vec::new();
    for row in rows {
        let op = super::repo_sqlite::operation_from_row(&row)?;
        if op
            .tx_output
            .as_ref()
            .is_some_and(super::terminal_adapter::pipe_worker::is_pipe)
        {
            continue;
        }
        let started = super::worker_cleanup::may_have_started(&op)?;
        if !started {
            continue;
        }
        let output = op.tx_output.as_ref().ok_or_else(|| {
            CalmError::Conflict(format!(
                "operation {} has no prepared launch identity; retain resources for reconciliation",
                op.id
            ))
        })?;
        let terminal_id = output.output_string("terminal_id", "terminal disposal")?;
        let state = RequestState::read(&output.data)?;
        let successful = matches!(op.phase, Phase::Succeeded | Phase::SpawnSucceeded);
        let socket = match state {
            Some(
                RequestState::Requested {
                    terminal_id: recorded,
                    supervisor_sock,
                    ..
                }
                | RequestState::Rejected {
                    terminal_id: recorded,
                    supervisor_sock,
                    ..
                },
            ) => {
                if recorded != terminal_id {
                    return Err(CalmError::Conflict(
                        "prepared launch terminal identity changed; retain resources".into(),
                    ));
                }
                Some(supervisor_sock)
            }
            Some(RequestState::HandedOff {
                terminal_id: recorded,
                supervisor_sock,
                ..
            }) => {
                if recorded != terminal_id {
                    return Err(CalmError::Conflict(
                        "handed off terminal identity changed; retain resources".into(),
                    ));
                }
                if successful || matches!(op.phase, Phase::SpawnStarted) {
                    continue;
                }
                // Compensation after a handed off launch is still unresolved;
                // a group kill acknowledgement never proves disposal safe.
                Some(supervisor_sock)
            }
            Some(RequestState::NotRequested { .. }) if op.kind != "codex-worker" || successful => {
                continue;
            }
            // Old successful operations predate this checkpoint; missing state at SpawnStarted is NOT success.
            None if successful => continue,
            _ => None,
        };
        unresolved.push(Unresolved {
            operation_id: op.id,
            terminal_id,
            socket,
        });
    }
    Ok(unresolved)
}

/// Transaction recheck immediately before removing rows or replacing workspace ownership; no supervisor I/O happens in this writer.
pub(crate) async fn require_safe_tx(tx: &mut Tx<'_>, scope: &Scope) -> Result<()> {
    if !unsettled(tx, scope).await?.is_empty() {
        return Err(CalmError::Conflict(
            "unsettled workspace executions; rows and workspace retained".into(),
        ));
    }
    require_legacy_safe_tx(tx, scope).await
}

async fn require_legacy_safe_tx(tx: &mut Tx<'_>, scope: &Scope) -> Result<()> {
    if let Some(unresolved) = unresolved_tx(tx, scope).await?.first() {
        return Err(unresolved.error());
    }
    Ok(())
}

/// Positive proof for Track filesystem effects; quiesce remains owner-scoped.
pub(crate) async fn require_track_location_safe_tx(tx: &mut Tx<'_>, track: &str) -> Result<()> {
    let workspace = crate::db::sqlite::track_workspace_read_tx(tx, track).await?;
    let targets = workspace_effect_targets(&workspace);
    require_location_safe_tx(
        tx,
        &Scope::Track(track.into()),
        calm_truth::db::sqlite::workspace_execution::Scope::Track(track),
        &targets,
    )
    .await
}

/// One positive proof, retaining the Legacy obligation check for the same scope.
pub(crate) async fn require_location_safe_tx(
    tx: &mut Tx<'_>,
    legacy_scope: &Scope,
    execution_scope: calm_truth::db::sqlite::workspace_execution::Scope<'_>,
    targets: &[PathBuf],
) -> Result<()> {
    let ids = calm_truth::db::sqlite::workspace_execution::location_occupancy(
        tx,
        execution_scope,
        targets,
    )
    .await?;
    if !ids.is_empty() {
        return Err(CalmError::Conflict(format!(
            "unsettled workspace executions {ids:?}; rows and workspace retained"
        )));
    }
    require_legacy_safe_tx(tx, legacy_scope).await
}

pub(crate) fn workspace_effect_targets(workspace: &crate::model::TrackWorkspace) -> Vec<PathBuf> {
    let mut targets = Vec::new();
    if workspace.kind == crate::model::TrackWorkspaceKind::Managed {
        targets.push(PathBuf::from(&workspace.path));
    }
    if let Some(worktree) = &workspace.worktree {
        targets.push(PathBuf::from(worktree));
    }
    targets
}

/// Inspect the entire affected scope before touching any member's resources; the durable obligation is kept regardless of reply, missing PID, renderer state or a truthful Probe(false).
pub(crate) async fn require_safe(
    repo: &dyn RouteRepo,
    scope: Scope,
    configured_socket: Option<&Path>,
) -> Result<()> {
    let unresolved = write_in_tx_typed(repo, move |tx| {
        Box::pin(async move { unresolved_tx(tx, &scope).await })
    })
    .await?;
    for launch in &unresolved {
        if let Some(socket) = launch.socket.as_deref().or(configured_socket) {
            crate::terminal_renderer::request_terminal_stop(socket, &launch.terminal_id).await;
        }
    }
    match unresolved.first() {
        Some(launch) => Err(launch.error()),
        None => Ok(()),
    }
}

pub(crate) async fn unsettled(
    tx: &mut Tx<'_>,
    scope: &Scope,
) -> Result<Vec<workspace::execution::ExecutionId>> {
    use calm_truth::db::sqlite::workspace_execution::{self as sql, Scope as ExecutionScope};
    let alias;
    let scope = match scope {
        Scope::Track(id) => ExecutionScope::Track(id),
        Scope::Area(id) => ExecutionScope::Area(id),
        Scope::Terminal(id) => {
            alias = sql::terminal_alias(id);
            ExecutionScope::Alias(&alias)
        }
        Scope::Card(id) => ExecutionScope::Card(id),
    };
    Ok(sql::unsettled_executions(tx, scope).await?)
}

/// Required-repository owner for every terminal deletion. Callers retain their scope and
/// location admission fences through the final transaction; no external I/O holds a writer.
pub(crate) async fn quiesce_terminal(
    repo: &dyn RouteRepo,
    engine: Option<&crate::execution_boot::ExecutionEngine>,
    renderer: &crate::terminal_renderer::TerminalRendererRegistry,
    configured_socket: Option<&Path>,
    term: &crate::model::Terminal,
) -> Result<()> {
    require_safe(repo, Scope::Terminal(term.id.clone()), configured_socket).await?;
    if term.is_typed(repo).await? {
        return crate::execution_quiesce::quiesce(repo, engine, Scope::Terminal(term.id.clone()))
            .await;
    }
    let socket = legacy_endpoint(repo, term, configured_socket).await?;
    let cfg = crate::terminal_renderer::RendererConfig::observation(term, socket)
        .map_err(|e| CalmError::Conflict(e.to_string()))?;
    renderer.quiesce_legacy(cfg).await.map_err(|error| {
        tracing::warn!(terminal_id=%term.id, %error, "legacy terminal disposal retained resources");
        CalmError::Conflict("Deletion blocked because the server could not confirm that an old terminal has exited.".into())
    })
}

async fn legacy_endpoint(
    repo: &dyn RouteRepo,
    term: &crate::model::Terminal,
    configured: Option<&Path>,
) -> Result<PathBuf> {
    let absolute = |p: &Path| -> Result<PathBuf> {
        Ok(if p.is_absolute() {
            p.into()
        } else {
            std::env::current_dir()?.join(p)
        })
    };
    let configured = configured.map(absolute).transpose()?;
    let scope = Scope::Terminal(term.id.clone());
    let terminal = term.id.clone();
    let recorded = write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            let (sql, id) = unresolved_query(&scope);
            let rows = sqlx::query(&sql).bind(id).fetch_all(&mut **tx).await?;
            let mut endpoint = None;
            for row in rows {
                let op = super::repo_sqlite::operation_from_row(&row)?;
                let Some(output) = op.tx_output.as_ref() else {
                    continue;
                };
                let recorded = match RequestState::read(&output.data)? {
                    Some(
                        RequestState::Requested {
                            terminal_id,
                            supervisor_sock,
                            ..
                        }
                        | RequestState::HandedOff {
                            terminal_id,
                            supervisor_sock,
                            ..
                        }
                        | RequestState::Rejected {
                            terminal_id,
                            supervisor_sock,
                            ..
                        },
                    ) => {
                        if terminal_id != terminal {
                            return Err(CalmError::Conflict(
                                "terminal launch identity changed".into(),
                            ));
                        }
                        Some(supervisor_sock)
                    }
                    _ => None,
                };
                if let Some(recorded) = recorded {
                    if endpoint
                        .as_ref()
                        .is_some_and(|previous| previous != &recorded)
                    {
                        return Err(CalmError::Conflict(
                            "terminal owner endpoints disagree".into(),
                        ));
                    }
                    endpoint = Some(recorded);
                }
            }
            Ok(endpoint)
        })
    })
    .await?
    .as_deref()
    .map(absolute)
    .transpose()?;
    if let (Some(configured), Some(recorded)) = (&configured, &recorded)
        && configured != recorded
    {
        return Err(CalmError::Conflict(
            "terminal launch endpoint disagrees with configuration".into(),
        ));
    }
    recorded.or(configured).ok_or_else(|| CalmError::Conflict(
        "Deletion blocked because the server could not confirm that an old terminal has exited.".into()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite::SqlxRepo;
    use workspace_runtime::execution::{Store, test_support};

    #[tokio::test]
    async fn legacy_disposal_typed_missing_renderer_requires_execution_settlement() {
        let repo = std::sync::Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let store = crate::execution_store::SqliteExecutionStore::new(repo.db().clone())
            .with_projection(
                test_support::TEST_CONSUMER.into(),
                std::sync::Arc::new(crate::execution_store::tests::NoopProjection),
            );
        let term = crate::model::Terminal {
            id: "typed-missing".into(),
            card_id: "typed-card".into(),
            program: "/bin/true".into(),
            cwd: "/".into(),
            env: serde_json::json!({}),
            pid: Some(123),
            theme_fg: "255,255,255".into(),
            theme_bg: "0,0,0".into(),
            exit_code: Some(2),
            signal_killed: false,
            pty_output: String::new(),
            pty_output_truncated: false,
            created_at: 0,
        };
        let mut intent = test_support::intent("/".into());
        intent.metadata.alias = workspace_runtime::execution::ProcessAlias(
            calm_truth::db::sqlite::workspace_execution::terminal_alias(&term.id),
        );
        let execution_id = intent.metadata.id.clone();
        let observed_store = store.clone();
        repo.db()
            .writer()
            .with_writer(|writer| {
                Box::pin(async move {
                    let mut tx = writer.begin_immediate().await.unwrap();
                    store.insert(&mut tx, &intent).await.unwrap();
                    tx.commit().await.unwrap();
                })
            })
            .await
            .unwrap();
        let renderer =
            crate::terminal_renderer::TerminalRendererRegistry::new_with_repo(repo.clone());
        assert!(matches!(
            quiesce_terminal(repo.as_ref(), None, &renderer, None, &term).await,
            Err(CalmError::Conflict(_))
        ));
        assert!(renderer.get(&term.id).is_none());
        assert!(matches!(
            observed_store.load(&execution_id).await.unwrap(),
            Some(workspace_runtime::execution::Stored::Active {
                desired: workspace_runtime::execution::Desired::Stop,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn terminal_disposal_lookup_seeks_terminal_identity() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let scope = Scope::Terminal("target-terminal".into());
        let (sql, id) = unresolved_query(&scope);
        let plan: Vec<(i64, i64, i64, String)> =
            sqlx::query_as(&format!("EXPLAIN QUERY PLAN {sql}"))
                .bind(id)
                .fetch_all(repo.pool())
                .await
                .unwrap();
        assert!(
            plan.iter()
                .any(|(_, _, _, detail)| detail.contains("SEARCH o USING INDEX")
                    && detail.contains("<expr>=?")),
            "terminal lookup must seek its identity rather than scan worker history: {plan:?}"
        );
    }

    #[tokio::test]
    async fn terminal_disposal_index_upgrade_preserves_existing_operations() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        (repo.db())
            .writer()
            .execute(sqlx::query("DROP INDEX operations_terminal_lookup_idx"))
            .await
            .unwrap();
        // Existing databases mix legacy launches, pipe tasks and pre-prepare rows.
        (repo.db()).writer().execute(sqlx::query(
            "WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<5315)
             INSERT INTO operations(id,operation_key,kind,payload_hash,target_type,
                target_json,payload_json,tx_output_json,phase,created_at_ms,updated_at_ms)
             SELECT printf('op-%d',i),printf('key-%d',i),'terminal-worker','hash','card','{}','{}',
                CASE i%3 WHEN 0 THEN json_object('data',json_object('terminal_id',printf('terminal-%d',i)))
                WHEN 1 THEN json_object('data',json_object('terminal_worker_pipe',json_object('log_path','task.log')))
                ELSE NULL END,'pending',0,0 FROM n",
        )).await.unwrap();
        let sql = "SELECT id,kind,tx_output_json,phase FROM operations ORDER BY id";
        let before: Vec<(String, String, Option<String>, String)> =
            sqlx::query_as(sql).fetch_all(repo.pool()).await.unwrap();
        (repo.db())
            .writer()
            .execute(sqlx::raw_sql(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../calm-truth/migrations/0207_operations_terminal_lookup.sql"
            ))))
            .await
            .unwrap();
        let after: Vec<(String, String, Option<String>, String)> =
            sqlx::query_as(sql).fetch_all(repo.pool()).await.unwrap();
        assert_eq!(
            after, before,
            "the lookup index must preserve legacy evidence"
        );
    }

    #[tokio::test]
    async fn require_safe_tx_rejects_unsettled_typed() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let store = crate::execution_store::SqliteExecutionStore::new(repo.db().clone())
            .with_projection(
                test_support::TEST_CONSUMER.into(),
                std::sync::Arc::new(crate::execution_store::tests::NoopProjection),
            );
        let intent = test_support::intent("/".into());
        let scope = Scope::Track(intent.metadata.track.0.clone());
        let write_db = (repo.db()).clone();
        write_db
            .writer()
            .with_writer(|write_session| {
                Box::pin(async move {
                    let mut tx = write_session.begin_immediate().await.unwrap();
                    store.insert(&mut tx, &intent).await.unwrap();
                    assert!(matches!(
                        require_safe_tx(&mut tx, &scope).await,
                        Err(CalmError::Conflict(_))
                    ));
                    tx.rollback().await.unwrap();
                    let mut tx = write_session.begin_immediate().await.unwrap();
                    require_safe_tx(&mut tx, &scope).await.unwrap();
                })
            })
            .await
            .expect("independent write operation")
    }
}
