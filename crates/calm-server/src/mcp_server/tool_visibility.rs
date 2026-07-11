//! #891 slice ④ — per-wave plugin tool visibility (closes #833 P2's minimal
//! multi-workflow isolation scope).
//!
//! A wave bound to a workflow (`wave.workflow_id = Some(..)`) must only see
//! and call the tools of the plugin that owns that workflow; kernel `calm.*`
//! registry tools stay role-gated as before and never route through here.
//! Unbound waves keep the historical union of all running plugins' tools —
//! but that policy also flows through [`plugin_scope_for_wave`] so the whole
//! visibility decision lives at a single choke point, applied on BOTH the
//! discovery path (`tools/list`) and the dispatch path (`tools/call`).
//!
//! Fail-closed (design §4 + 决策记录 F7): when a wave is bound to a workflow
//! whose owning plugin cannot be resolved from the running ∧ trusted set
//! (plugin stopped, trust revoked, wave row unreadable), the scope is
//! [`WavePluginScope::None`] — zero plugin tools. This mirrors the spec
//! harness's descriptor-unresolved degradation (vanilla prompt): the tools
//! are withdrawn together with the workflow context rather than silently
//! widened back to the union.

use std::sync::Arc;

use crate::forge_trust::trusted_forge_plugin;
use crate::mcp_server::registry::AppContext;

/// Which plugins' tools a caller may see / call, resolved from the caller's
/// wave context. Produced only by [`plugin_scope_for_wave`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WavePluginScope {
    /// No wave context (pre-attribution discovery) or an unbound wave —
    /// union of all running plugins (historical behavior, pinned by tests).
    All,
    /// Wave bound to a workflow — only the running trusted plugin that
    /// declares that workflow.
    Only(String /* plugin_id */),
    /// Wave bound to a workflow whose owning plugin is currently
    /// unresolvable (stopped / untrusted / wave lookup failed) — zero
    /// plugin tools, fail-closed.
    None,
}

impl WavePluginScope {
    pub(crate) fn allows(&self, plugin_id: &str) -> bool {
        match self {
            Self::All => true,
            Self::Only(allowed) => allowed == plugin_id,
            Self::None => false,
        }
    }
}

/// Single choke-point policy: resolve the plugin-tool scope for a caller.
///
/// * `wave_id = None` (no wave context) → [`WavePluginScope::All`].
/// * Wave row has `workflow_id = None` (unbound) → [`WavePluginScope::All`].
/// * Wave bound to `workflow_id = Some(wf)` → [`WavePluginScope::Only`] of
///   the running ∧ trusted plugin whose manifest declares `wf` (same filter
///   as `routes::waves::resolve_trusted_workflow` and the spec harness's
///   `bound_workflow`); [`WavePluginScope::None`] when no such plugin.
/// * Wave lookup failure / missing wave row → [`WavePluginScope::None`]:
///   bound-ness cannot be proven, so fail closed rather than widen to the
///   union.
pub(crate) async fn plugin_scope_for_wave(
    ctx: &Arc<AppContext>,
    wave_id: Option<&str>,
) -> WavePluginScope {
    // #891 review fix (hot-path observability): this resolver sits on both
    // the tools/list and tools/call paths and does per-call repo + registry
    // reads; log the resolution at debug so latency regressions and scope
    // decisions are attributable without enabling caching in this slice.
    let started = std::time::Instant::now();
    let scope = resolve_plugin_scope_for_wave(ctx, wave_id).await;
    tracing::debug!(
        target: "mcp_server::tool_visibility",
        wave_id = wave_id.unwrap_or("<none>"),
        scope = ?scope,
        elapsed_us = started.elapsed().as_micros() as u64,
        "plugin tool scope resolved"
    );
    scope
}

async fn resolve_plugin_scope_for_wave(
    ctx: &Arc<AppContext>,
    wave_id: Option<&str>,
) -> WavePluginScope {
    let Some(wave_id) = wave_id else {
        return WavePluginScope::All;
    };
    let wave = match ctx.repo.wave_get(wave_id).await {
        Ok(Some(wave)) => wave,
        Ok(None) => {
            tracing::warn!(
                target: "mcp_server::tool_visibility",
                wave_id,
                "plugin tool scope: wave not found; failing closed (no plugin tools)"
            );
            return WavePluginScope::None;
        }
        Err(error) => {
            tracing::warn!(
                target: "mcp_server::tool_visibility",
                wave_id,
                error = %error,
                "plugin tool scope: wave lookup failed; failing closed (no plugin tools)"
            );
            return WavePluginScope::None;
        }
    };
    let Some(workflow_id) = wave.workflow_id.as_deref() else {
        // Unbound wave — historical union, but routed through this function
        // so the policy has exactly one home.
        return WavePluginScope::All;
    };
    let Some(plugin_host) = ctx.plugin_host.get().cloned() else {
        // Bound wave but no plugin host yet (boot ordering) — there are no
        // plugin tools to expose anyway; report the fail-closed scope.
        return WavePluginScope::None;
    };
    let running_plugin_ids = plugin_host.running_plugin_ids().await;
    for manifest in plugin_host.registry().list() {
        if !running_plugin_ids.contains(&manifest.id) || !trusted_forge_plugin(&manifest.id) {
            continue;
        }
        if manifest
            .workflows
            .iter()
            .any(|workflow| workflow.id == workflow_id)
        {
            return WavePluginScope::Only(manifest.id);
        }
    }
    tracing::warn!(
        target: "mcp_server::tool_visibility",
        wave_id,
        workflow_id,
        "plugin tool scope: bound workflow has no running trusted owner; failing closed"
    );
    WavePluginScope::None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::card_role_cache::CardRoleCache;
    use crate::db::prelude::*;
    use crate::db::sqlite::SqlxRepo;
    use crate::event::EventBus;
    use crate::forge_trust::trusted_forge_plugin;
    use crate::model::{NewCove, NewWave};
    use crate::plugin_host::{Manifest, PluginHost, PluginRegistry, PluginRuntimeStatus};
    use crate::routes::theme::RequestTheme;
    use crate::state::WriteContext;
    use crate::wave_cove_cache::WaveCoveCache;
    use serde_json::json;
    use std::path::PathBuf;
    use std::time::Duration;
    use tokio::time::{Instant, sleep};

    const WORKFLOW_ID: &str = "tool-visibility-flow";

    #[test]
    fn scope_allows_matrix() {
        assert!(WavePluginScope::All.allows("any.plugin"));
        assert!(WavePluginScope::Only("dev.owner".into()).allows("dev.owner"));
        assert!(!WavePluginScope::Only("dev.owner".into()).allows("dev.other"));
        assert!(!WavePluginScope::None.allows("dev.owner"));
    }

    #[tokio::test]
    async fn scope_matrix_none_unbound_bound_and_missing() {
        let trusted_plugin_id = configured_trusted_plugin_id();
        let repo = Arc::new(
            SqlxRepo::open("sqlite::memory:")
                .await
                .expect("open in-memory sqlite"),
        );
        let bound_wave = make_wave(repo.as_ref(), Some(WORKFLOW_ID)).await;
        let unbound_wave = make_wave(repo.as_ref(), None).await;

        // Trusted plugin declaring the workflow, RUNNING → Only.
        let (host, _tmp) = plugin_host_with_workflow(repo.clone(), &trusted_plugin_id).await;
        host.spawn(&trusted_plugin_id)
            .await
            .expect("spawn trusted plugin");
        wait_for_running(&host, &trusted_plugin_id).await;
        let ctx = app_context(repo.clone(), Some(host.clone()));

        // No wave context → All.
        assert_eq!(
            plugin_scope_for_wave(&ctx, None).await,
            WavePluginScope::All
        );
        // Unbound wave → All (union regression pin).
        assert_eq!(
            plugin_scope_for_wave(&ctx, Some(unbound_wave.id.as_str())).await,
            WavePluginScope::All
        );
        // Bound wave, running trusted owner → Only(owner).
        assert_eq!(
            plugin_scope_for_wave(&ctx, Some(bound_wave.id.as_str())).await,
            WavePluginScope::Only(trusted_plugin_id.clone())
        );
        // Missing wave row → fail-closed None.
        assert_eq!(
            plugin_scope_for_wave(&ctx, Some("no-such-wave")).await,
            WavePluginScope::None
        );

        host.stop(&trusted_plugin_id)
            .await
            .expect("stop trusted plugin");

        // Trusted plugin registered but STOPPED → fail-closed None.
        assert_eq!(
            plugin_scope_for_wave(&ctx, Some(bound_wave.id.as_str())).await,
            WavePluginScope::None
        );
        // Unbound wave stays All even with the owner stopped.
        assert_eq!(
            plugin_scope_for_wave(&ctx, Some(unbound_wave.id.as_str())).await,
            WavePluginScope::All
        );
    }

    #[tokio::test]
    async fn bound_wave_with_untrusted_declaring_plugin_fails_closed() {
        let untrusted_plugin_id = untrusted_plugin_id();
        let repo = Arc::new(
            SqlxRepo::open("sqlite::memory:")
                .await
                .expect("open in-memory sqlite"),
        );
        let bound_wave = make_wave(repo.as_ref(), Some(WORKFLOW_ID)).await;

        let (host, _tmp) = plugin_host_with_workflow(repo.clone(), &untrusted_plugin_id).await;
        host.spawn(&untrusted_plugin_id)
            .await
            .expect("spawn untrusted plugin");
        wait_for_running(&host, &untrusted_plugin_id).await;
        let ctx = app_context(repo.clone(), Some(host.clone()));

        assert_eq!(
            plugin_scope_for_wave(&ctx, Some(bound_wave.id.as_str())).await,
            WavePluginScope::None
        );

        host.stop(&untrusted_plugin_id)
            .await
            .expect("stop untrusted plugin");
    }

    #[tokio::test]
    async fn bound_wave_without_plugin_host_fails_closed() {
        let repo = Arc::new(
            SqlxRepo::open("sqlite::memory:")
                .await
                .expect("open in-memory sqlite"),
        );
        let bound_wave = make_wave(repo.as_ref(), Some(WORKFLOW_ID)).await;
        let unbound_wave = make_wave(repo.as_ref(), None).await;
        let ctx = app_context(repo, None);

        assert_eq!(
            plugin_scope_for_wave(&ctx, Some(bound_wave.id.as_str())).await,
            WavePluginScope::None
        );
        assert_eq!(
            plugin_scope_for_wave(&ctx, Some(unbound_wave.id.as_str())).await,
            WavePluginScope::All
        );
    }

    fn configured_trusted_plugin_id() -> String {
        std::env::var("NEIGE_TRUSTED_FORGE_PLUGINS")
            .ok()
            .and_then(|configured| {
                configured
                    .split(',')
                    .map(str::trim)
                    .find(|id| !id.is_empty())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "dev.neige.git-forge".to_string())
    }

    fn untrusted_plugin_id() -> String {
        let mut candidate = "dev.neige.untrusted-visibility-test".to_string();
        let mut suffix = 0;
        while trusted_forge_plugin(&candidate) {
            suffix += 1;
            candidate = format!("dev.neige.untrusted-visibility-test-{suffix}");
        }
        candidate
    }

    fn app_context(repo: Arc<SqlxRepo>, host: Option<Arc<PluginHost>>) -> Arc<AppContext> {
        let repo_dyn: Arc<dyn crate::db::Repo> = repo;
        let route_repo: Arc<dyn crate::db::RouteRepo> = repo_dyn;
        let plugin_host = Arc::new(tokio::sync::OnceCell::new());
        if let Some(host) = host {
            assert!(
                plugin_host.set(host).is_ok(),
                "late-bound plugin host cell must be set once"
            );
        }
        Arc::new(AppContext {
            repo: route_repo,
            wave_vcs: None,
            events: EventBus::new(),
            write: WriteContext::new(CardRoleCache::new(), WaveCoveCache::new()),
            daemon_token_hash: None,
            gate_logs_dir: std::env::temp_dir().join("neige-test-gate-logs"),
            plugin_host,
            operation_runtime: Arc::new(tokio::sync::OnceCell::new()),
        })
    }

    async fn make_wave(repo: &SqlxRepo, workflow_id: Option<&str>) -> crate::model::Wave {
        let cove = repo
            .cove_create(NewCove {
                name: format!("cove-{workflow_id:?}"),
                color: "#101010".into(),
                sort: None,
            })
            .await
            .expect("create cove");
        repo.wave_create(NewWave {
            workflow_input: None,
            cove_id: cove.id,
            title: "tool visibility".into(),
            sort: None,
            cwd: String::new(),
            workflow_id: workflow_id.map(str::to_string),
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .expect("create wave")
    }

    async fn plugin_host_with_workflow(
        repo: Arc<SqlxRepo>,
        plugin_id: &str,
    ) -> (Arc<PluginHost>, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let plugins_dir = tmp.path().join("plugins");
        let plugins_data_dir = tmp.path().join("plugins-data");
        let install_dir = plugins_dir.join(plugin_id);
        let bin_dir = install_dir.join("bin");
        std::fs::create_dir_all(&bin_dir).expect("create plugin bin dir");
        std::fs::create_dir_all(&plugins_data_dir).expect("create plugins data dir");
        std::os::unix::fs::symlink(stub_echo_bin(), bin_dir.join("stub"))
            .expect("symlink echo stub");

        let manifest_json = json!({
            "manifest_version": 1,
            "id": plugin_id,
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Tool Visibility Stub",
            "entrypoint": { "command": "bin/stub" },
            "workflows": [
                {
                    "id": WORKFLOW_ID,
                    "plan_template": [
                        {
                            "key": "inspect",
                            "kind": "codex",
                            "goal": "Inspect the issue.",
                            "depends_on": []
                        }
                    ],
                    "gates": [],
                    "spec_instructions": "Use workflow {wave_id}.",
                    "card_kinds": []
                }
            ],
            "permissions": {}
        });
        let manifest = Manifest::parse(&manifest_json.to_string()).expect("manifest parses");
        let registry = PluginRegistry::empty();
        registry.insert(manifest, Some(install_dir.clone()));
        repo.plugin_install(crate::model::NewPlugin {
            id: plugin_id.to_string(),
            version: "0.1.0".into(),
            install_path: install_dir.display().to_string(),
            manifest: manifest_json,
            enabled: true,
            user_config: json!({}),
        })
        .await
        .expect("seed plugin row");
        let repo_dyn: Arc<dyn crate::db::Repo> = repo;
        let host = Arc::new(PluginHost::new_full(
            Arc::new(registry),
            repo_dyn,
            plugins_dir,
            plugins_data_dir,
            Vec::new(),
            EventBus::new(),
            WriteContext::new(CardRoleCache::new(), WaveCoveCache::new()),
        ));
        (host, tmp)
    }

    async fn wait_for_running(host: &Arc<PluginHost>, plugin_id: &str) {
        let start = Instant::now();
        loop {
            if let Some(status) = host.status(plugin_id).await
                && matches!(status.status, PluginRuntimeStatus::Running)
            {
                return;
            }
            assert!(
                start.elapsed() < Duration::from_secs(2),
                "timed out waiting for plugin {plugin_id} to run"
            );
            sleep(Duration::from_millis(25)).await;
        }
    }

    fn stub_echo_bin() -> PathBuf {
        if let Some(path) = std::env::var_os("CARGO_BIN_EXE_plugin-host-stub-echo") {
            return path.into();
        }
        if let Some(path) = option_env!("CARGO_BIN_EXE_plugin-host-stub-echo") {
            return path.into();
        }
        let current = std::env::current_exe().expect("current test executable");
        let deps_dir = current.parent().expect("test executable parent");
        let debug_dir = deps_dir.parent().expect("target debug dir");
        let candidate = debug_dir.join("plugin-host-stub-echo");
        assert!(
            candidate.exists(),
            "missing plugin-host-stub-echo at {}",
            candidate.display()
        );
        candidate
    }
}
