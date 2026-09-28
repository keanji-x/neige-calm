//! Whether the Claude Planner can run right now (#1817), and the CLI's model list that the same
//! check fetches and caches (#1822). The first failed step is the reason: `--claude-planner-config`
//! is present (else `not_configured`), the pinned binary answers `--version` with the pinned
//! version, `auth status --json` says `loggedIn`, then `initialize` lists the models.
//!
//! The list is fetched only when none is cached (the boot check, the first read, or any check
//! after one that was not ready) and on an explicit recheck. The
//! [`TTL`](crate::agent_providers::TTL) re-check of version and
//! login keeps it: the binary is pinned, so the list changes only with the account or its
//! entitlement, and a person then presses Recheck.

use std::sync::Arc;

use super::catalog_fetch::{CATALOG_TIMEOUT, fetch};
use super::config::{CONFIG_FLAG, ClaudePlannerHost};
use super::models::ClaudeCatalog;
use crate::agent_providers::{CheckRun, Checked, Freshness, Stamped, Verdict};
use crate::session_projection_repo::AgentProvider;

/// One Claude check's outcome. Only a ready Claude has a catalog, and a ready one always has one.
#[derive(Debug, Clone)]
pub enum ClaudeReadiness {
    Ready(Arc<ClaudeCatalog>),
    Unavailable(String),
    NotConfigured(String),
}

impl Stamped<ClaudeReadiness> {
    /// The provider-neutral answer `GET /api/agent-providers` reports.
    pub fn checked(&self) -> Checked {
        let verdict = match &self.outcome {
            ClaudeReadiness::Ready(_) => Verdict::Ready,
            ClaudeReadiness::Unavailable(reason) => Verdict::Unavailable(reason.clone()),
            ClaudeReadiness::NotConfigured(reason) => Verdict::NotConfigured(reason.clone()),
        };
        Checked {
            provider: AgentProvider::Claude,
            verdict,
            checked_at_ms: self.checked_at_ms,
        }
    }

    /// The catalog, or the refusal of a caller that needs Claude now: [`Checked::require_ready`]'s.
    pub fn catalog(&self) -> Result<Arc<ClaudeCatalog>, String> {
        match &self.outcome {
            ClaudeReadiness::Ready(catalog) => Ok(Arc::clone(catalog)),
            ClaudeReadiness::Unavailable(_) | ClaudeReadiness::NotConfigured(_) => Err(self
                .checked()
                .require_ready()
                .expect_err("a check that is not ready refuses")),
        }
    }
}

/// One Claude check, given the previous outcome of its cache slot; `freshness` is why it runs
/// (`Cached`: the TTL expired). See the module docs for when it re-fetches the model list. Only a
/// TTL re-check that kept the cached list cannot answer a recheck.
pub(crate) async fn check(
    host: &ClaudePlannerHost,
    previous: Option<ClaudeReadiness>,
    freshness: Freshness,
) -> CheckRun<ClaudeReadiness> {
    let reusable = match (freshness, previous) {
        (Freshness::Cached, Some(ClaudeReadiness::Ready(catalog))) => Some(catalog),
        _ => None,
    };
    let answers_recheck = reusable.is_none();
    CheckRun {
        outcome: run(host, reusable).await,
        answers_recheck,
    }
}

async fn run(host: &ClaudePlannerHost, reusable: Option<Arc<ClaudeCatalog>>) -> ClaudeReadiness {
    let Ok(config) = host.configured() else {
        return ClaudeReadiness::NotConfigured(format!(
            "calm-server was started without {CONFIG_FLAG}; restart it with \
             {CONFIG_FLAG} <file> to run Claude Planners"
        ));
    };
    let env = match host.readiness_env(config) {
        Ok(env) => env,
        Err(error) => {
            return ClaudeReadiness::Unavailable(format!(
                "the Claude Planner environment could not be built: {error}"
            ));
        }
    };
    if let Some(problem) = config.version_problem(&env).await {
        return ClaudeReadiness::Unavailable(format!(
            "{problem}; point `claude_binary` and `claude_version` in the {CONFIG_FLAG} file \
             at an installed version"
        ));
    }
    match super::auth_status::logged_in(
        &config.claude_binary,
        &env,
        super::auth_status::AUTH_STATUS_TIMEOUT,
    )
    .await
    {
        Ok(true) => {}
        Ok(false) => {
            return ClaudeReadiness::Unavailable(format!(
                "not logged in — run `claude /login` with CLAUDE_CONFIG_DIR={}",
                config.config_dir.display()
            ));
        }
        Err(reason) => return ClaudeReadiness::Unavailable(reason),
    }
    // The TTL re-check keeps the list a ready check cached; a recheck, or no list, fetches it.
    if let Some(catalog) = reusable {
        return ClaudeReadiness::Ready(catalog);
    }
    match fetch(
        &config.claude_binary,
        &env,
        &host.instructions_dir,
        CATALOG_TIMEOUT,
    )
    .await
    {
        Ok(catalog) => ClaudeReadiness::Ready(Arc::new(catalog)),
        Err(reason) => ClaudeReadiness::Unavailable(format!(
            "{reason}; a Claude Planner runs only a model the CLI lists — recheck once it lists them"
        )),
    }
}
