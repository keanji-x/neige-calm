//! Whether each Planner provider can run right now, and why not (#1817). One check per provider,
//! the first failure being the reason; `GET /api/agent-providers`, track create and the boot log
//! all read it through [`ProviderAvailabilityCache`].
//!
//! Codex: the shared app-server is running, then `account/read` says it is logged in.
//! Claude: `--claude-planner-config` is present (else `not_configured`), the pinned binary answers
//! `--version` with the pinned version, then `auth status --json` says `loggedIn`.
//! Neither check reads a credential file, and no account identity leaves this module.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::time::Instant;
use utoipa::ToSchema;

use crate::claude_planner::config::{CONFIG_FLAG, ClaudePlannerHost};
use crate::session_projection_repo::AgentProvider;
use crate::shared_codex_appserver::SharedCodexAppServer;

/// How long a check answers for before the next reader runs it again.
pub const TTL: Duration = Duration::from_secs(30);

/// The budget of one `account/read`.
const CODEX_ACCOUNT_READ_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderStatus {
    /// Every check passed.
    Ready,
    /// A check failed; `reason` says which and how to fix it.
    Unavailable,
    /// This server has no such backend (a Claude Planner without `--claude-planner-config`).
    NotConfigured,
}

/// One provider's current availability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ProviderAvailability {
    pub provider: AgentProvider,
    pub status: ProviderStatus,
    /// neige's own sentence naming the failed check and its fix; `null` exactly when `ready`.
    #[schema(required = true)]
    pub reason: Option<String>,
    /// Wall-clock ms at which the check that produced this answer started.
    pub checked_at_ms: i64,
}

/// A check's outcome; every non-ready outcome carries its reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Ready,
    Unavailable(String),
    NotConfigured(String),
}

/// One check's result as the cache holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    pub provider: AgentProvider,
    pub verdict: Verdict,
    /// Wall-clock ms at which the check started.
    pub checked_at_ms: i64,
}

impl Checked {
    /// `Err` is the refusal of a caller that needs this provider now: its wire name and reason.
    pub fn require_ready(&self) -> std::result::Result<(), String> {
        match &self.verdict {
            Verdict::Ready => Ok(()),
            Verdict::Unavailable(reason) | Verdict::NotConfigured(reason) => {
                let name = match self.provider {
                    AgentProvider::Codex => "codex",
                    AgentProvider::Claude => "claude",
                };
                Err(format!("`{name}` is unavailable: {reason}"))
            }
        }
    }
}

impl From<Checked> for ProviderAvailability {
    fn from(checked: Checked) -> Self {
        let (status, reason) = match checked.verdict {
            Verdict::Ready => (ProviderStatus::Ready, None),
            Verdict::Unavailable(reason) => (ProviderStatus::Unavailable, Some(reason)),
            Verdict::NotConfigured(reason) => (ProviderStatus::NotConfigured, Some(reason)),
        };
        Self {
            provider: checked.provider,
            status,
            reason,
            checked_at_ms: checked.checked_at_ms,
        }
    }
}

/// Whether a reader accepts an answer younger than [`TTL`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    Cached,
    /// Run the check again (`?refresh=true`), unless one started after this request did.
    Recheck,
}

struct Entry {
    started: Instant,
    checked: Checked,
}

/// One slot per provider. A slot's lock is held across its check, so concurrent readers share
/// one check in flight rather than each spawning their own.
#[derive(Default)]
pub struct ProviderAvailabilityCache {
    codex: tokio::sync::Mutex<Option<Entry>>,
    claude: tokio::sync::Mutex<Option<Entry>>,
}

impl ProviderAvailabilityCache {
    /// `provider`'s availability: the cached answer when `freshness` allows and it is younger
    /// than [`TTL`], else a new check against the backends passed in.
    pub async fn get(
        &self,
        provider: &AgentProvider,
        freshness: Freshness,
        claude: &ClaudePlannerHost,
        codex: &SharedCodexAppServer,
    ) -> Checked {
        let requested = Instant::now();
        let mut slot = match provider {
            AgentProvider::Codex => self.codex.lock().await,
            AgentProvider::Claude => self.claude.lock().await,
        };
        if let Some(entry) = slot.as_ref() {
            // A check that began after this request answers it, even a recheck.
            let began_after_request = entry.started >= requested;
            let fresh = freshness == Freshness::Cached && entry.started.elapsed() < TTL;
            if began_after_request || fresh {
                return entry.checked.clone();
            }
        }
        let started = Instant::now();
        let checked_at_ms = crate::model::now_ms();
        let verdict = match provider {
            AgentProvider::Codex => check_codex(codex).await,
            AgentProvider::Claude => check_claude(claude).await,
        };
        let checked = Checked {
            provider: provider.clone(),
            verdict,
            checked_at_ms,
        };
        *slot = Some(Entry {
            started,
            checked: checked.clone(),
        });
        checked
    }

    /// Every provider: Codex, then Claude.
    pub async fn all(
        &self,
        freshness: Freshness,
        claude: &ClaudePlannerHost,
        codex: &SharedCodexAppServer,
    ) -> Vec<Checked> {
        let (codex_checked, claude_checked) = tokio::join!(
            self.get(&AgentProvider::Codex, freshness, claude, codex),
            self.get(&AgentProvider::Claude, freshness, claude, codex),
        );
        vec![codex_checked, claude_checked]
    }
}

async fn check_codex(daemon: &SharedCodexAppServer) -> Verdict {
    if !daemon.is_running() {
        return Verdict::Unavailable(daemon.not_running_message());
    }
    match daemon
        .account_read(Instant::now() + CODEX_ACCOUNT_READ_TIMEOUT)
        .await
    {
        Ok(account) if account.logged_in() => Verdict::Ready,
        Ok(_) => Verdict::Unavailable(format!(
            "codex is not logged in — run `codex login` with CODEX_HOME={}",
            daemon.codex_home_path().display()
        )),
        Err(error) => Verdict::Unavailable(format!(
            "could not ask the shared codex app-server whether it is logged in ({error}); \
             recheck shortly"
        )),
    }
}

async fn check_claude(host: &ClaudePlannerHost) -> Verdict {
    let Ok(config) = host.configured() else {
        return Verdict::NotConfigured(format!(
            "calm-server was started without {CONFIG_FLAG}; restart it with \
             {CONFIG_FLAG} <file> to run Claude Planners"
        ));
    };
    let env = match host.readiness_env(config) {
        Ok(env) => env,
        Err(error) => {
            return Verdict::Unavailable(format!(
                "the Claude Planner environment could not be built: {error}"
            ));
        }
    };
    if let Some(problem) = config.version_problem(&env).await {
        return Verdict::Unavailable(format!(
            "{problem}; point `claude_binary` and `claude_version` in the {CONFIG_FLAG} file \
             at an installed version"
        ));
    }
    match crate::claude_planner::auth_status::logged_in(
        &config.claude_binary,
        &env,
        crate::claude_planner::auth_status::AUTH_STATUS_TIMEOUT,
    )
    .await
    {
        Ok(true) => Verdict::Ready,
        Ok(false) => Verdict::Unavailable(format!(
            "not logged in — run `claude /login` with CLAUDE_CONFIG_DIR={}",
            config.config_dir.display()
        )),
        Err(reason) => Verdict::Unavailable(reason),
    }
}

/// The boot check: one pass over every provider, off the boot path; each non-ready provider is
/// a warning, never a boot failure.
pub fn spawn_boot_check(state: &crate::state::AppState) -> tokio::task::JoinHandle<()> {
    let route = <crate::state::RouteState as axum::extract::FromRef<_>>::from_ref(state);
    let codex = state.shared_codex_appserver.clone();
    tokio::spawn(async move {
        for checked in route
            .provider_availability
            .all(Freshness::Recheck, &route.claude_planner, &codex)
            .await
        {
            if let Verdict::Unavailable(reason) = &checked.verdict {
                tracing::warn!(
                    provider = ?checked.provider,
                    reason,
                    "planner provider unavailable at boot"
                );
            }
        }
    })
}
