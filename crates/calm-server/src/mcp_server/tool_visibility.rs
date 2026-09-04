//! #891 slice ④ / #1110 S4 — per-track plugin tool visibility.
//!
//! A track with `plugin_scope = Some(plugin_id)` must only see and call that
//! plugin's tools; kernel `calm.*` registry tools stay role-gated as before
//! and never route through here. Unbound tracks (`plugin_scope = None`) keep
//! the historical union of all running plugins' tools — but that policy also
//! flows through [`plugin_scope_for_track`] so the whole visibility decision
//! lives at a single choke point, applied on BOTH the discovery path
//! (`tools/list`) and the dispatch path (`tools/call`).
//!
//! Fail-closed (design §4 + 决策记录 F7 / #1110 S4): when a track is scoped
//! to a plugin that is not currently running ∧ trusted (plugin stopped,
//! trust revoked, track row unreadable), the scope is
//! [`TrackPluginScope::None`] — zero plugin tools. This mirrors the planner
//! harness's descriptor-unresolved degradation (vanilla prompt): the tools
//! are withdrawn together with the plugin context rather than silently
//! widened back to the union.
//!
//! #1321 S1 — the paragraph above used to end "The gate reads
//! `tracks.plugin_scope` only — it does not look up `templates[]` by
//! `template_id`". The first half still holds and is now stronger: the owner
//! column is the *only* way in, and this module no longer decides that by
//! itself — it delegates to [`crate::track_binding::resolve_track_owner_binding`],
//! the single per-track owner judgement shared with the planner harness. The
//! second half no longer describes the code: the resolver *does* consult
//! `templates[]`, not to find an owner, but to check that the recorded owner
//! still declares the track's `template_id` and still accepts its persisted
//! `template_input`. Both checks fail closed here for the same reason a
//! stopped owner does — a track whose template contract has drifted out from
//! under it has no coherent plugin context left to scope tools to.

use std::sync::Arc;

use crate::mcp_server::registry::AppContext;
use crate::track_binding::{TrackOwnerBinding, resolve_track_owner_binding};

/// Which plugins' tools a caller may see / call, resolved from the caller's
/// track context. Produced only by [`plugin_scope_for_track`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TrackPluginScope {
    /// No track context (pre-attribution discovery) or an unbound track —
    /// union of all running plugins (historical behavior, pinned by tests).
    All,
    /// Track with `plugin_scope = Some(id)` whose plugin is running ∧ trusted.
    Only(String /* plugin_id */),
    /// Track with `plugin_scope` set whose plugin is currently unresolvable
    /// (stopped / untrusted / track lookup failed) — zero plugin tools,
    /// fail-closed.
    None,
}

impl TrackPluginScope {
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
/// * `track_id = None` (no track context) → [`TrackPluginScope::All`].
/// * Track row has `plugin_scope = None` (unbound) → [`TrackPluginScope::All`].
/// * Track has `plugin_scope = Some(id)` → [`TrackPluginScope::Only`] if
///   [`resolve_track_owner_binding`] resolves that owner (running ∧ trusted ∧
///   still declaring the track's `template_id` ∧ still accepting its
///   persisted `template_input`); [`TrackPluginScope::None`] otherwise.
/// * Track lookup failure / missing track row → [`TrackPluginScope::None`]:
///   bound-ness cannot be proven, so fail closed rather than widen to the
///   union.
///
/// #1321 S1 — the decision itself lives in [`crate::track_binding`]; this
/// function only projects it onto the tool-visibility vocabulary, so it
/// cannot drift from the planner harness's answer.
pub(crate) async fn plugin_scope_for_track(
    ctx: &Arc<AppContext>,
    track_id: Option<&str>,
) -> TrackPluginScope {
    // #891 review fix (hot-path observability): this resolver sits on both
    // the tools/list and tools/call paths and does per-call repo + registry
    // reads; log the resolution at debug so latency regressions and scope
    // decisions are attributable without enabling caching in this slice.
    let started = std::time::Instant::now();
    let scope = resolve_plugin_scope_for_track(ctx, track_id).await;
    tracing::debug!(
        target: "mcp_server::tool_visibility",
        track_id = track_id.unwrap_or("<none>"),
        scope = ?scope,
        elapsed_us = started.elapsed().as_micros() as u64,
        "plugin tool scope resolved"
    );
    scope
}

async fn resolve_plugin_scope_for_track(
    ctx: &Arc<AppContext>,
    track_id: Option<&str>,
) -> TrackPluginScope {
    let Some(track_id) = track_id else {
        return TrackPluginScope::All;
    };
    let track = match ctx.repo.track_get(track_id).await {
        Ok(Some(track)) => track,
        Ok(None) => {
            tracing::warn!(
                target: "mcp_server::tool_visibility",
                track_id,
                "plugin tool scope: track not found; failing closed (no plugin tools)"
            );
            return TrackPluginScope::None;
        }
        Err(error) => {
            tracing::warn!(
                target: "mcp_server::tool_visibility",
                track_id,
                error = %error,
                "plugin tool scope: track lookup failed; failing closed (no plugin tools)"
            );
            return TrackPluginScope::None;
        }
    };
    let plugin_host = ctx.plugin_host.get().cloned();
    match resolve_track_owner_binding(&track, plugin_host.as_deref()).await {
        // Unbound track — historical union, but routed through the shared
        // resolver so the policy has exactly one home.
        TrackOwnerBinding::Unbound => TrackPluginScope::All,
        TrackOwnerBinding::Owned { plugin, .. } => TrackPluginScope::Only(plugin.id),
        TrackOwnerBinding::FailedClosed(failure) => {
            tracing::warn!(
                target: "mcp_server::tool_visibility",
                track_id,
                plugin_id = failure.plugin_id().unwrap_or("<none>"),
                failure = %failure,
                "plugin tool scope: the track's recorded owner did not resolve; failing closed"
            );
            TrackPluginScope::None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::card_role_cache::CardRoleCache;
    use crate::db::prelude::*;
    use crate::db::sqlite::SqlxRepo;
    use crate::event::EventBus;
    use crate::forge_trust::trusted_forge_plugin;
    use crate::model::{NewArea, NewTrack};
    use crate::plugin_host::{Manifest, PluginHost, PluginRegistry, PluginRuntimeStatus};
    use crate::routes::theme::RequestTheme;
    use crate::state::WriteContext;
    use crate::track_area_cache::TrackAreaCache;
    use serde_json::json;
    use std::path::PathBuf;
    use std::time::Duration;
    use tokio::time::{Instant, sleep};

    const TEMPLATE_ID: &str = "tool-visibility-flow";

    #[test]
    fn scope_allows_matrix() {
        assert!(TrackPluginScope::All.allows("any.plugin"));
        assert!(TrackPluginScope::Only("dev.owner".into()).allows("dev.owner"));
        assert!(!TrackPluginScope::Only("dev.owner".into()).allows("dev.other"));
        assert!(!TrackPluginScope::None.allows("dev.owner"));
    }

    #[tokio::test]
    async fn scope_matrix_none_unbound_bound_and_missing() {
        let trusted_plugin_id = configured_trusted_plugin_id();
        let repo = Arc::new(
            SqlxRepo::open("sqlite::memory:")
                .await
                .expect("open in-memory sqlite"),
        );
        let bound_track = make_track(repo.as_ref(), Some(trusted_plugin_id.as_str())).await;
        let unbound_track = make_track(repo.as_ref(), None).await;

        // Trusted plugin RUNNING → Only.
        let (host, _tmp) = plugin_host_with_template(repo.clone(), &trusted_plugin_id).await;
        host.spawn(&trusted_plugin_id)
            .await
            .expect("spawn trusted plugin");
        wait_for_running(&host, &trusted_plugin_id).await;
        let ctx = app_context(repo.clone(), Some(host.clone()));

        // No track context → All.
        assert_eq!(
            plugin_scope_for_track(&ctx, None).await,
            TrackPluginScope::All
        );
        // Unbound track → All (union regression pin).
        assert_eq!(
            plugin_scope_for_track(&ctx, Some(unbound_track.id.as_str())).await,
            TrackPluginScope::All
        );
        // Bound track, running trusted owner → Only(owner).
        assert_eq!(
            plugin_scope_for_track(&ctx, Some(bound_track.id.as_str())).await,
            TrackPluginScope::Only(trusted_plugin_id.clone())
        );
        // Missing track row → fail-closed None.
        assert_eq!(
            plugin_scope_for_track(&ctx, Some("no-such-track")).await,
            TrackPluginScope::None
        );

        // #1110 S4 flatten pin: template_id alone is not the gate even
        // when the matching plugin is running.
        let leftover_template = repo
            .track_create(crate::model::NewTrack {
                template_input: None,
                area_id: unbound_track.area_id.clone(),
                title: "template-id leftover".into(),
                sort: None,
                cwd: String::new(),
                template_id: Some(TEMPLATE_ID.into()),
                plugin_scope: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            })
            .await
            .expect("create leftover-template track");
        assert_eq!(
            plugin_scope_for_track(&ctx, Some(leftover_template.id.as_str())).await,
            TrackPluginScope::All
        );

        host.stop(&trusted_plugin_id)
            .await
            .expect("stop trusted plugin");

        // Trusted plugin registered but STOPPED → fail-closed None.
        assert_eq!(
            plugin_scope_for_track(&ctx, Some(bound_track.id.as_str())).await,
            TrackPluginScope::None
        );
        // Unbound track stays All even with the owner stopped.
        assert_eq!(
            plugin_scope_for_track(&ctx, Some(unbound_track.id.as_str())).await,
            TrackPluginScope::All
        );
    }

    #[tokio::test]
    async fn bound_track_with_untrusted_declaring_plugin_fails_closed() {
        let untrusted_plugin_id = untrusted_plugin_id();
        let repo = Arc::new(
            SqlxRepo::open("sqlite::memory:")
                .await
                .expect("open in-memory sqlite"),
        );
        let bound_track = make_track(repo.as_ref(), Some(untrusted_plugin_id.as_str())).await;

        let (host, _tmp) = plugin_host_with_template(repo.clone(), &untrusted_plugin_id).await;
        host.spawn(&untrusted_plugin_id)
            .await
            .expect("spawn untrusted plugin");
        wait_for_running(&host, &untrusted_plugin_id).await;
        let ctx = app_context(repo.clone(), Some(host.clone()));

        assert_eq!(
            plugin_scope_for_track(&ctx, Some(bound_track.id.as_str())).await,
            TrackPluginScope::None
        );

        host.stop(&untrusted_plugin_id)
            .await
            .expect("stop untrusted plugin");
    }

    #[tokio::test]
    async fn bound_track_without_plugin_host_fails_closed() {
        let repo = Arc::new(
            SqlxRepo::open("sqlite::memory:")
                .await
                .expect("open in-memory sqlite"),
        );
        let bound_track = make_track(repo.as_ref(), Some("dev.neige.git-forge")).await;
        let unbound_track = make_track(repo.as_ref(), None).await;
        let ctx = app_context(repo, None);

        assert_eq!(
            plugin_scope_for_track(&ctx, Some(bound_track.id.as_str())).await,
            TrackPluginScope::None
        );
        assert_eq!(
            plugin_scope_for_track(&ctx, Some(unbound_track.id.as_str())).await,
            TrackPluginScope::All
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
            track_vcs: None,
            events: EventBus::new(),
            write: WriteContext::new(CardRoleCache::new(), TrackAreaCache::new()),
            daemon_token_hash: None,
            gate_logs_dir: std::env::temp_dir().join("neige-test-gate-logs"),
            plugin_host,
            operation_runtime: Arc::new(tokio::sync::OnceCell::new()),
        })
    }

    async fn make_track(repo: &SqlxRepo, plugin_scope: Option<&str>) -> crate::model::Track {
        let area = repo
            .area_create(NewArea {
                name: format!("area-{plugin_scope:?}"),
                color: "#101010".into(),
                sort: None,
            })
            .await
            .expect("create area");
        repo.track_create(NewTrack {
            template_input: None,
            area_id: area.id,
            title: "tool visibility".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: plugin_scope.map(str::to_string),
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .expect("create track")
    }

    async fn plugin_host_with_template(
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
            "manifest_version": 2,
            "id": plugin_id,
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Tool Visibility Stub",
            "entrypoint": { "command": "bin/stub" },
            "templates": [
                { "id": TEMPLATE_ID }
            ],
            "permissions": {}
        });
        let manifest = Manifest::parse(&manifest_json.to_string()).expect("manifest parses");
        let registry = PluginRegistry::from_manifests([(manifest, Some(install_dir.clone()))]);
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
            WriteContext::new(CardRoleCache::new(), TrackAreaCache::new()),
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
