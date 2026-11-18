//! Planner policy and compatibility names over the shared ACP host.
use super::profile::AcpAgentProfile;
pub(crate) use crate::acp_session::host::Registration;
pub use crate::acp_session::host::{
    AcpAgentConfig, AcpHost as AcpPlannerHost, AcpPlannerConfig, MARKER_KEY,
};
use crate::error::{CalmError, Result};
use calm_types::runtime::AgentProtocol;

#[cfg(test)]
mod authentication_tests;
/// Revoke owned credentials before listening, even without a current registration. A failed
/// pre-C4 marker sweep warns and retains an unproven-session fence; runtime Stop is durable.
pub async fn boot(repo: &dyn crate::db::Repo, host: &AcpPlannerHost) -> Result<()> {
    boot_with_sweep(repo, host, |ids| async move {
        let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        crate::planner_process::sweep_pre_c4(&host.instance, &ids).await
    })
    .await
}

#[cfg(all(test, feature = "fixtures"))]
pub(crate) async fn boot_with_empty_marker_scan_for_test(
    repo: &dyn crate::db::Repo,
    host: &AcpPlannerHost,
) -> Result<()> {
    boot_with_sweep(repo, host, |_| async { Ok(()) }).await
}

pub(crate) async fn boot_with_sweep<F: std::future::Future<Output = Result<()>>>(
    repo: &dyn crate::db::Repo,
    host: &AcpPlannerHost,
    sweep: impl FnOnce(Vec<String>) -> F,
) -> Result<()> {
    let ids = revoke_owned(repo, host.native_workers.clone(), None).await?;
    let pass_a_ids = ids.clone();
    if let Err(error) = crate::db::write_in_tx_typed(repo, move |tx| {
        Box::pin(
            async move { crate::provider_execution::sweep::boot_pass_a(tx, &pass_a_ids).await },
        )
    })
    .await
    {
        tracing::warn!(%error, "ACP boot pass A failed; durable sweep will retry");
    }
    let mut markers = ids.clone();
    markers.push(super::catalog::WORKER.into());
    markers.push("readiness".into());
    let shared: Vec<_> = calm_types::runtime::AgentProtocol::Acp
        .providers()
        .filter(|provider| {
            super::profile::AcpAgentProfile::of(provider)
                .is_some_and(|profile| profile.resident.is_some())
        })
        .collect();
    markers.extend(
        shared
            .iter()
            .map(|provider| provider.wire_name().to_owned()),
    );
    let sweep_confirmed = match sweep(markers).await {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(%error, "ACP boot pre-C4 sweep did not confirm; old MCP credentials revoked; boot continues");
            false
        }
    };
    host.native_workers.recover(repo, sweep_confirmed).await?;
    let unproven = if sweep_confirmed {
        Default::default()
    } else {
        ids.into_iter().collect()
    };
    host.pre_c4
        .set(crate::provider_execution::PreC4Sweep { unproven })
        .map_err(|_| CalmError::Conflict("ACP boot sweep proof already initialized".into()))?;
    Ok(())
}

/// Reconcile receipts after the execution owner drains previous resident scopes.
pub(crate) async fn recover_residents_after_boot(
    repo: &dyn crate::db::Repo,
    host: &AcpPlannerHost,
) -> Result<()> {
    for provider in AgentProtocol::Acp.providers().filter(|provider| {
        AcpAgentProfile::of(provider).is_some_and(|profile| profile.resident.is_some())
    }) {
        super::session::recover_shared_after_boot(repo, provider, host.pre_c4.get()).await?;
    }
    Ok(())
}

/// Refuse before the route can supersede, shut down, or request any Stop.
/// The boot-owned unproven set is fixed; later sessions are proven by construction.
/// The sweep still rechecks inside its credential revocation transaction.
pub(crate) async fn preflight_track(
    repo: &dyn crate::db::Repo,
    host: &AcpPlannerHost,
    track: &str,
) -> Result<()> {
    let db = repo.sqlite_db().ok_or(CalmError::ExecutionUnavailable)?;
    let ids = crate::db::sqlite::acp_managed_session_ids(db.read_pool(), Some(track)).await?;
    crate::provider_execution::PreC4Sweep::require_absent(host.pre_c4.get(), &ids)
}

pub async fn sweep_track(
    repo: &dyn crate::db::RepoEventWrite,
    host: &AcpPlannerHost,
    track: &str,
) -> Result<()> {
    sweep_tracks(repo, host, [track]).await
}

pub(crate) async fn sweep_tracks<'a>(
    repo: &dyn crate::db::RepoEventWrite,
    host: &AcpPlannerHost,
    tracks: impl IntoIterator<Item = &'a str>,
) -> Result<()> {
    let tracks: Vec<_> = tracks.into_iter().map(str::to_owned).collect();
    let proof = host.pre_c4.get().cloned();
    let native_workers = host.native_workers.clone();
    let ids = crate::db::write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            let mut ids = Vec::new();
            for track in tracks {
                let owned = revoke_owned_tx(tx, native_workers.as_ref(), Some(&track)).await?;
                // A refusal rolls back revocation before any Stop can be requested.
                crate::provider_execution::PreC4Sweep::require_absent(proof.as_ref(), &owned)?;
                ids.extend(owned);
                ids.extend(
                    calm_truth::db::sqlite::acp_resident::workers_for_track(tx, &track).await?,
                );
            }
            ids.sort_unstable();
            ids.dedup();
            Ok(ids)
        })
    })
    .await?;
    let workers: Vec<_> = ids.iter().map(String::as_str).collect();
    super::session::drain_workers(host, &workers).await?;
    let executions = crate::db::write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            let mut executions = Vec::new();
            for session in ids {
                if let Some(claim) =
                    calm_truth::db::sqlite::provider_execution::load(&mut **tx, &session).await?
                    && let Some(id) = crate::provider_execution::request_stop_tx(
                        tx,
                        &session,
                        &claim,
                        crate::provider_execution::StopReason::TrackRetired,
                    )
                    .await?
                {
                    executions.push(id);
                }
            }
            Ok(executions)
        })
    })
    .await?;
    if executions.is_empty() {
        return Ok(());
    }
    let engine = crate::provider_execution::engine(host.execution.engine())?;
    crate::execution_quiesce::drive(Some(engine), executions.iter().cloned());
    tokio::time::timeout(workspace_runtime::execution::QUIESCE_DEADLINE, async {
        for id in executions {
            crate::provider_execution::wait_settled(
                engine,
                &id,
                workspace_runtime::execution::QUIESCE_DEADLINE,
            )
            .await?;
        }
        Ok::<_, CalmError>(())
    })
    .await
    .map_err(crate::provider_execution::conflict)?
}

async fn revoke_owned(
    repo: &dyn crate::db::RepoEventWrite,
    native_workers: std::sync::Arc<dyn crate::acp_session::roles::NativeWorkers>,
    track: Option<String>,
) -> Result<Vec<String>> {
    crate::db::write_in_tx_typed(repo, move |tx| {
        Box::pin(
            async move { revoke_owned_tx(tx, native_workers.as_ref(), track.as_deref()).await },
        )
    })
    .await
}

async fn revoke_owned_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    native_workers: &dyn crate::acp_session::roles::NativeWorkers,
    track: Option<&str>,
) -> Result<Vec<String>> {
    let mut ids = crate::db::sqlite::acp_managed_session_ids(&mut **tx, track).await?;
    ids.extend(native_workers.revoke_owned_tx(tx, track).await?);
    sqlx::query(
        r#"UPDATE worker_sessions
              SET mcp_token_hash = NULL
            WHERE id IN (SELECT worker_session_id FROM acp_managed_sessions)
              AND (?1 IS NULL OR track_id = ?1)"#,
    )
    .bind(track)
    .execute(&mut **tx)
    .await?;
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;

    fn read(agent: serde_json::Value) -> Result<AcpPlannerConfig> {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), json!({"agents":[agent]}).to_string()).unwrap();
        AcpPlannerConfig::read(file.path())
    }
    fn opencode() -> serde_json::Value {
        json!({"provider":"opencode","command":"/opt/opencode","args":[],"env":{},
            "min_agent_version":"2.1.3"})
    }

    #[test]
    fn opencode_refuses_operator_config_content_instead_of_overwriting_it() {
        let mut agent = opencode();
        agent["env"] = json!({"OPENCODE_CONFIG_CONTENT":"{\"model\":\"private/model\"}"});
        let error = read(agent).expect_err("configuration collision must be refused");
        assert!(
            error.to_string().contains("OPENCODE_CONFIG_CONTENT"),
            "{error}"
        );
    }

    #[test]
    fn creation_permission_mode_is_optional_and_strict() {
        assert_eq!(
            read(opencode()).unwrap().agents[0].default_permission_mode,
            None
        );
        for mode in ["never", "ask", "full"] {
            let mut agent = opencode();
            agent["default_permission_mode"] = json!(mode);
            assert_eq!(
                serde_json::to_value(
                    read(agent).unwrap().agents[0]
                        .default_permission_mode
                        .unwrap()
                )
                .unwrap(),
                json!(mode)
            );
        }
        for invalid in [
            json!("Full"),
            json!("unknown"),
            json!(null),
            json!(true),
            json!(1),
            json!({"full":null}),
            json!([]),
        ] {
            let mut agent = opencode();
            agent["default_permission_mode"] = invalid.clone();
            assert!(read(agent).is_err(), "{invalid}");
        }
    }

    #[test]
    fn a_registration_reads_its_minimum_version_as_semver() {
        let config = read(opencode()).unwrap();
        assert_eq!(
            config.agents[0].min_agent_version,
            semver::Version::new(2, 1, 3)
        );
        for version in ["2.1", "v2.1.3", "latest", ""] {
            let mut agent = opencode();
            agent["min_agent_version"] = json!(version);
            assert!(read(agent).is_err(), "{version}");
        }
        let mut agent = opencode();
        agent.as_object_mut().unwrap().remove("min_agent_version");
        assert!(read(agent).is_err(), "min_agent_version is required");
        // The ACP entry is the profile's; a registration cannot name it.
        let mut agent = opencode();
        agent["subcommand"] = json!(["acp"]);
        assert!(read(agent).is_err(), "subcommand is unknown");
    }

    /// An older file names what replaced its fields, beside the parse error and its position, and
    /// the old and new forms are never accepted together.
    #[test]
    fn replaced_identity_fields_are_refused_with_their_replacement() {
        for (field, value, hint) in [
            (
                "expected_agent_version",
                "2.1.3",
                "rename it to `min_agent_version`",
            ),
            (
                "expected_agent_name",
                "OpenCode",
                "the provider's profile fixes",
            ),
        ] {
            // Alone, as an older file has it with its `acp` argument, and beside its replacement.
            for keep_new_form in [false, true] {
                let mut agent = opencode();
                if !keep_new_form {
                    agent.as_object_mut().unwrap().remove("min_agent_version");
                    agent["args"] = json!(["acp"]);
                }
                agent[field] = json!(value);
                let message = read(agent).unwrap_err().to_string();
                assert!(
                    message.contains(field)
                        && message.contains(hint)
                        && message.contains("`args` no longer holds the ACP subcommand")
                        && message.contains("at line 1 column"),
                    "{message}"
                );
            }
        }
    }

    /// A file as older versions wrote it is told every change at once.
    #[test]
    fn an_older_file_is_told_every_replaced_field() {
        let mut agent = opencode();
        let object = agent.as_object_mut().unwrap();
        object.remove("min_agent_version");
        object.insert("args".into(), json!(["acp"]));
        object.insert("expected_agent_name".into(), json!("OpenCode"));
        object.insert("expected_agent_version".into(), json!("2.1.2"));
        let message = read(agent).unwrap_err().to_string();
        assert!(
            message.contains("`expected_agent_name` was replaced; remove it")
                && message.contains("`expected_agent_version` was replaced; rename it"),
            "{message}"
        );
    }

    /// A parse error of a current file keeps its position.
    #[test]
    fn a_parse_error_names_its_position() {
        let mut agent = opencode();
        agent["min_agent_version"] = json!("latest");
        let message = read(agent).unwrap_err().to_string();
        assert!(message.contains("at line 1 column"), "{message}");
    }

    /// A default registration may not hand Grok an xAI API key: it uses the signed-in
    /// subscription. Every other provider keeps its environment as registered.
    #[test]
    fn a_grok_registration_refuses_xai_api_keys() {
        let grok = |env: serde_json::Value| {
            json!({"provider":"grok","command":"/opt/grok/grok-1.0.62","args":[],"env":env,
                "min_agent_version":"1.0.62"})
        };
        assert!(read(grok(json!({"GROK_HOME":"/var/lib/neige/grok"}))).is_ok());
        for key in ["XAI_API_KEY", "GROK_CODE_XAI_API_KEY"] {
            let message = read(grok(json!({ key: "xai-secret" })))
                .unwrap_err()
                .to_string();
            assert!(
                message.contains(&format!("cannot set `{key}`")) && !message.contains("xai-secret"),
                "{message}"
            );
        }
        let mut agent = opencode();
        agent["env"] = json!({"XAI_API_KEY":"unrelated"});
        assert!(read(agent).is_ok());
    }

    /// Only ACP providers register, each at most once.
    #[test]
    fn a_registration_names_each_acp_provider_at_most_once() {
        let grok = json!({"provider":"grok","command":"/opt/grok","args":[],"env":{},
            "min_agent_version":"1.0.62"});
        let file = tempfile::NamedTempFile::new().unwrap();
        let write = |agents: serde_json::Value| {
            std::fs::write(file.path(), json!({ "agents": agents }).to_string()).unwrap();
            AcpPlannerConfig::read(file.path())
        };
        assert!(write(json!([opencode(), grok.clone()])).is_ok());
        assert!(write(json!([grok.clone(), grok])).is_err());
        let mut codex = opencode();
        codex["provider"] = json!("codex");
        assert!(write(json!([codex])).is_err());
    }

    /// The native session key covers the agent process and its directory, not admission policy.
    #[test]
    fn only_the_agent_process_and_cwd_key_a_native_session() {
        let digest = |agent: serde_json::Value, cwd: &str| {
            read(agent).unwrap().agents[0]
                .native_session_digest(Path::new(cwd))
                .unwrap()
        };
        let base = digest(opencode(), "/work");
        for mode in ["never", "ask", "full"] {
            let mut agent = opencode();
            agent["default_permission_mode"] = json!(mode);
            assert_eq!(
                digest(agent, "/work"),
                base,
                "default_permission_mode={mode}"
            );
        }
        let mut raised = opencode();
        raised["min_agent_version"] = json!("1.19.0");
        assert_eq!(digest(raised, "/work"), base, "min_agent_version");
        for (field, value) in [
            ("command", json!("/opt/other/opencode")),
            ("args", json!(["--wrapped"])),
            ("env", json!({"HOME":"/private/other"})),
        ] {
            let mut changed = opencode();
            changed[field] = value;
            assert_ne!(digest(changed, "/work"), base, "{field}");
        }
        assert_ne!(digest(opencode(), "/elsewhere"), base, "cwd");
    }
}
