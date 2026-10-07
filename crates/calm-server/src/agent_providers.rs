//! Whether each Planner provider can run right now, and why not (#1817). One check per provider,
//! the first failure being the reason; `GET /api/agent-providers`, track create and the boot log
//! all read it through [`ProviderAvailabilityCache`].
//!
//! Codex: the shared app-server is running, then `account/read` says it is logged in.
//! Claude (`claude_planner::availability`): `--claude-planner-config` is present (else
//! `not_configured`), the pinned binary answers `--version` with the pinned version,
//! `auth status --json` says `loggedIn`, then the CLI lists its models (#1822).
//! Neither check reads a credential file, and no account identity leaves either module.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::time::Instant;
use utoipa::ToSchema;

use crate::claude_planner::availability::ClaudeReadiness;
use crate::claude_planner::config::ClaudePlannerHost;
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
                let name = self.provider.wire_name();
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

/// A check's outcome and the wall-clock ms at which that check started.
#[derive(Debug, Clone)]
pub struct Stamped<T> {
    pub outcome: T,
    pub checked_at_ms: i64,
}

struct Entry<T> {
    started: Instant,
    stamped: Stamped<T>,
}

/// One provider's last check. The lock is held across a check, so concurrent readers share one
/// check in flight rather than each spawning their own.
pub struct Slot<T> {
    entry: tokio::sync::Mutex<Option<Entry<T>>>,
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Self {
            entry: tokio::sync::Mutex::new(None),
        }
    }
}

impl<T> std::fmt::Debug for Slot<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Slot").finish_non_exhaustive()
    }
}

impl<T: Clone> Slot<T> {
    /// The cached answer when `freshness` allows and it is younger than [`TTL`], else
    /// `check(previous outcome)`'s, which replaces it.
    pub async fn get<F, Fut>(&self, freshness: Freshness, check: F) -> Stamped<T>
    where
        F: FnOnce(Option<T>) -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        let requested = Instant::now();
        let mut slot = self.entry.lock().await;
        if let Some(entry) = slot.as_ref() {
            // A check that began after this request answers it, even a recheck.
            let began_after_request = entry.started >= requested;
            let fresh = freshness == Freshness::Cached && entry.started.elapsed() < TTL;
            if began_after_request || fresh {
                return entry.stamped.clone();
            }
        }
        let started = Instant::now();
        let checked_at_ms = crate::model::now_ms();
        let previous = slot.as_ref().map(|entry| entry.stamped.outcome.clone());
        let stamped = Stamped {
            outcome: check(previous).await,
            checked_at_ms,
        };
        *slot = Some(Entry {
            started,
            stamped: stamped.clone(),
        });
        stamped
    }

    /// Fixtures only: make the cached answer older than [`TTL`], so the next cached read checks
    /// again as it would 30 s later.
    #[cfg(feature = "fixtures")]
    pub async fn age_past_ttl_for_test(&self) {
        if let Some(entry) = self.entry.lock().await.as_mut() {
            entry.started = entry
                .started
                .checked_sub(TTL + Duration::from_secs(1))
                .expect("the monotonic clock is older than the TTL");
        }
    }
}

/// One slot per provider. Claude's also holds the model list its check fetches (#1822).
#[derive(Default)]
pub struct ProviderAvailabilityCache {
    codex: Slot<Verdict>,
    claude: Slot<ClaudeReadiness>,
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
        match provider {
            AgentProvider::Codex => {
                let stamped = self
                    .codex
                    .get(freshness, |_previous| check_codex(codex))
                    .await;
                let authentication_failed = codex.authentication_failure().is_some();
                Checked {
                    provider: AgentProvider::Codex,
                    verdict: if authentication_failed {
                        Verdict::Unavailable(crate::codex_authentication::SIGN_IN_REQUIRED.into())
                    } else {
                        stamped.outcome
                    },
                    checked_at_ms: stamped.checked_at_ms,
                }
            }
            AgentProvider::Claude => self.claude(freshness, claude).await.checked(),
        }
    }

    /// The Claude check with the model list it caches: the cached answer when `freshness` allows
    /// and it is younger than [`TTL`], else a new check (see `claude_planner::availability` for
    /// when that re-fetches the list).
    pub async fn claude(
        &self,
        freshness: Freshness,
        host: &ClaudePlannerHost,
    ) -> Stamped<ClaudeReadiness> {
        self.claude
            .get(freshness, |previous| {
                crate::claude_planner::availability::check(host, previous, freshness)
            })
            .await
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

    /// Fixtures only: see [`Slot::age_past_ttl_for_test`].
    #[cfg(feature = "fixtures")]
    pub async fn age_past_ttl_for_test(&self) {
        self.codex.age_past_ttl_for_test().await;
        self.claude.age_past_ttl_for_test().await;
    }
}

async fn check_codex(daemon: &SharedCodexAppServer) -> Verdict {
    if !daemon.is_running_per_readiness() {
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
