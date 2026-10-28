//! The worker's launch environment (§3.4.1 as revised by the owner's WX1 decision). Three parts,
//! each with one source:
//!
//! - ambient: the supervisor merges its own child allowlist (`calm_proc_supervisor::ambient_keys`,
//!   the table the Legacy PTY spawn applies) at spawn, from its own environment, and never
//!   journals the values. Its proxy keys are excluded;
//! - kernel: the card and hook keys (`claude_card_env`, shared with the Legacy spawn), the MCP
//!   socket, the kernel-led PATH, the config source, and the kernel's resolved proxy values;
//! - release: the MCP token and every proxy value classified secret leave the kernel only in the
//!   Held stdin payload (`provider_execution::launch::Release`), never in argv, the execution
//!   specification, or any persisted record.
//!
//! The launch contract below has no place for a token: [`WorkerLaunch::intent`] takes it as an
//! argument, and WA2-D mints it in the admission transaction.
use super::role::WorkerArgs;
use crate::claude_planner::role::{ConfigSource, LaunchRole};
use crate::error::Result;
use crate::execution_environment::PROXY_KEYS;
use crate::operation::claude_adapter::claude_card_env;
use crate::provider_execution::{self as execution, ProviderExecutionOwner};
use calm_session::control::execution::{AmbientEnvironment, AmbientProxyPolicy};
use std::ffi::OsString;
use std::path::Path;
use workspace_runtime::execution::{ExecutionId, ExecutionIntent};

/// The kernel's part of a worker's environment.
pub(crate) struct WorkerEnvironment<'a> {
    pub config_source: ConfigSource<'a>,
    /// The kernel-led PATH (`kernel_bin_path::kernel_led_path`), so `neige` is this kernel's CLI.
    pub path: OsString,
    pub card_id: &'a str,
    /// The hook bridge's kernel URL (`CodexClient::ingest_url`).
    pub base_url: &'a str,
    pub mcp_socket: &'a Path,
    /// [`kernel_proxy`]'s values, public and secret alike; the release split separates them.
    pub proxy: Vec<(String, OsString)>,
}

impl WorkerEnvironment<'_> {
    /// The supervisor's allowlist without its proxies: every proxy comes from [`kernel_proxy`], so a
    /// secret one travels in the payload instead of being refused at spawn.
    pub(crate) fn ambient() -> AmbientEnvironment {
        AmbientEnvironment::SupervisorAllowed {
            proxy_policy: AmbientProxyPolicy::Exclude,
        }
    }

    /// Every kernel-valued entry before the release split.
    pub(crate) fn kernel_env(&self) -> Vec<(String, OsString)> {
        let mut env: Vec<(String, OsString)> = claude_card_env(self.card_id, self.base_url)
            .into_iter()
            .map(|(key, value)| (key.to_string(), value.into()))
            .collect();
        env.push((
            "NEIGE_MCP_SOCKET".into(),
            self.mcp_socket.as_os_str().to_owned(),
        ));
        env.push(("PATH".into(), self.path.clone()));
        env.extend(self.config_source.env());
        env.extend(self.proxy.iter().cloned());
        env
    }
}

/// The kernel's proxy values for a worker: the settings' HTTP and HTTPS proxies over the kernel's
/// own (the Planner's resolver), then each remaining proxy key from the kernel's environment.
pub(crate) fn kernel_proxy(
    http_settings: Option<&str>,
    https_settings: Option<&str>,
    lookup: impl Fn(&str) -> Option<String> + Copy,
) -> Vec<(String, OsString)> {
    let mut proxy: Vec<(String, OsString)> = Vec::new();
    for (upper, lower, value) in
        crate::proxy_env::resolved_proxy_env_pairs(http_settings, https_settings, lookup)
    {
        proxy.push((upper.into(), value.clone().into()));
        proxy.push((lower.into(), value.into()));
    }
    for key in PROXY_KEYS {
        if !proxy.iter().any(|(set, _)| set == key)
            && let Some(value) = lookup(key)
        {
            proxy.push(((*key).into(), value.into()));
        }
    }
    proxy
}

/// The worker's kernel MCP credential, held for one release. Neither `Debug` nor serializable.
pub(crate) struct WorkerMcpToken(String);
impl WorkerMcpToken {
    pub(crate) fn new(raw: String) -> Self {
        Self(raw)
    }
}

/// One worker turn's launch contract: binary, argv and environment. A turn has no token until
/// [`Self::intent`] receives one.
pub(crate) struct WorkerLaunch<'a> {
    /// `claude_version::resolve_claude_bin(&codex.claude_bin)`, not the Planner's pinned binary.
    pub binary: &'a Path,
    pub args: WorkerArgs<'a>,
    pub environment: WorkerEnvironment<'a>,
}

impl WorkerLaunch<'_> {
    /// The execution intent and its Held release payload. Only the payload carries the token and
    /// the secret proxy values; the intent is what the Engine and supervisor journal.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn intent(
        &self,
        owner: &dyn ProviderExecutionOwner,
        session: &str,
        track: &str,
        cwd: &Path,
        id: &ExecutionId,
        dir: &Path,
        token: WorkerMcpToken,
    ) -> Result<(ExecutionIntent, Vec<u8>)> {
        let release = execution::launch::Release::split(
            self.environment.kernel_env(),
            Some(token.0),
            "worker",
        )?;
        execution::launch::intent(
            owner,
            session,
            track,
            cwd,
            id,
            dir,
            self.binary,
            self.args.argv()?,
            release.public,
            &release.read_prefix,
            release.payload,
            WorkerEnvironment::ambient(),
        )
        .await
    }
}
