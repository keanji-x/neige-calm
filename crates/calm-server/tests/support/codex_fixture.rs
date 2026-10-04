use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::harness::HarnessRegistry;
use calm_server::ids::{AreaId, CardId, TrackId};
use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::{McpServer, ToolRegistry};
use calm_server::operation::OperationRuntime;
use calm_server::plugin_host::PluginHost;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{CodexClient, DaemonClient, WriteContext};
use calm_server::templates::ISSUE_DEVELOPMENT;
use calm_server::terminal_renderer::TerminalRendererRegistry;
use calm_server::track_area_cache::TrackAreaCache;
use serde_json::{Value, json};
use tempfile::TempDir;

use super::agent_diag::EvidenceTempDir;
use super::event_queries::*;
use super::forge_env::{EnvGuard, ForgeTestEnv};

mod boot;
mod host;
mod waits;

// Some test binaries that include `support` use nothing from `boot` or `waits`, and CI clippy runs
// with -D warnings; `host::*` needs no allow because this root module uses it.
#[allow(unused_imports)]
pub use boot::*;
pub use host::*;
#[allow(unused_imports)]
pub use waits::*;

pub const DEFAULT_PROXY: &str = "http://127.0.0.1:2080";
pub const FORGE_BIN: &str = env!("CARGO_BIN_EXE_git-forge");
pub const PLUGIN_ID: &str = "dev.neige.git-forge";
pub const COMMIT_TOOL: &str = "plugin.dev.neige.git-forge_git.commit";
pub const TASK_KEY: &str = "forge-e2e";
pub const PLANNER_SESSION_ID: &str = "codex-forge-e2e-planner-session";
/// The GitHub repository the fixture checkout's origin names; the local bare origin serves it.
pub const FIXTURE_GITHUB_REPO: &str = "neige-e2e/forge-fixture";

/// The fixture origin's configured URL: GitHub's for [`FIXTURE_GITHUB_REPO`].
pub fn fixture_github_url() -> String {
    format!("https://github.com/{FIXTURE_GITHUB_REPO}.git")
}

/// The command the issue-development repo cross-check runs; its first line is the configured origin
/// URL, before any `url.<base>.insteadOf` rewrite.
pub const REPO_CROSS_CHECK_CMD: &str = "git config --get-all remote.origin.url";

/// The builtin issue-development working method.
pub fn issue_development_method() -> String {
    calm_server::templates::TemplateRoster::builtin()
        .get(ISSUE_DEVELOPMENT)
        .expect("builtin issue-development template")
        .recipe()
        .body
}

/// The template's repo cross-check run in `repo`: `method` must still name the first line of
/// [`REPO_CROSS_CHECK_CMD`]; returns owner/name of the command's first line.
pub fn cross_checked_origin_repo(method: &str, repo: &Path) -> String {
    assert!(
        method.contains(&format!("first line of `{REPO_CROSS_CHECK_CMD}`")),
        "the issue-development method no longer names the first line of `{REPO_CROSS_CHECK_CMD}`"
    );
    let mut argv = REPO_CROSS_CHECK_CMD.split(' ');
    let output = StdCommand::new(argv.next().expect("program"))
        .args(argv)
        .current_dir(repo)
        .output()
        .expect("run the repo cross-check");
    assert!(
        output.status.success(),
        "{REPO_CROSS_CHECK_CMD} failed in {}",
        repo.display()
    );
    let urls = String::from_utf8_lossy(&output.stdout);
    let first = urls.lines().next().expect("origin has a url");
    first
        .trim_start_matches("https://github.com/")
        .trim_end_matches(".git")
        .to_string()
}

/// Valid issue-development `template_input` for `issue_number` of [`FIXTURE_GITHUB_REPO`], with
/// `merge_policy` `auto-merge` so a run merges without a ratification.
pub fn issue_development_input(issue_number: u64) -> Value {
    json!({
        "issue_url": format!("https://github.com/{FIXTURE_GITHUB_REPO}/issues/{issue_number}"),
        "repo": FIXTURE_GITHUB_REPO,
        "issue_number": issue_number,
        "merge_policy": "auto-merge",
    })
}

pub struct FixtureSpec {
    pub goal: Option<String>,
    /// Bind the track to the issue-development template for this issue (see
    /// [`issue_development_input`]); the Planner card carries the template's working method.
    pub bound_issue: Option<u64>,
    pub plan_source: PlanSource,
    pub issue_body: Option<FixtureIssue>,
    pub require_task_gates: bool,
    pub repo_seed: RepoSeed,
}

pub struct FixtureIssue {
    pub number: u64,
    pub body: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepoSeed {
    ReadmeOnly,
    /// A real Rust micro-crate with a hermetic rustc gate script and NO Cargo.toml.
    RustMicroCrate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanSource {
    Injected,
    RealPlannerTurn,
}

pub struct Fixture {
    pub server: Arc<McpServer>,
    pub plugin_host: Arc<PluginHost>,
    pub repo: Arc<SqlxRepo>,
    pub repo_dyn: Arc<dyn Repo>,
    pub events: EventBus,
    pub write: WriteContext,
    pub cache: CardRoleCache,
    pub track_area_cache: TrackAreaCache,
    pub area_id: AreaId,
    pub track_id: TrackId,
    pub planner_card_id: CardId,
    pub report_card_id: CardId,
    pub codex: Arc<CodexClient>,
    pub daemon: Arc<DaemonClient>,
    pub shared: Arc<SharedCodexAppServer>,
    pub runtime: Arc<OperationRuntime>,
    pub harness: HarnessRegistry,
    pub renderer: Arc<TerminalRendererRegistry>,
    pub ctx: Arc<AppContext>,
    pub registry: Arc<ToolRegistry>,
    pub used_injected_plan: AtomicBool,
    pub track_cwd: PathBuf,
    pub origin_repo: PathBuf,
    /// Kernel MCP UDS socket, exposed so tests can drive scripted `tools/call`s over the same wire real agent sessions use.
    pub socket_path: PathBuf,
    /// Plaintext shared-daemon MCP token (only the hash reaches the server).
    pub daemon_token: String,
    pub origin_main_initial: String,
    pub codex_stderr_log: PathBuf,
    pub _forge_env: ForgeTestEnv,
    pub _codex_path: EnvGuard,
    pub _proxy_env: ProxyEnv,
    pub _events_prune_env: EnvGuard,
    pub _tmp: EvidenceTempDir,
    pub _socket_tmp: TempDir,
}

impl Fixture {
    pub fn used_injected_plan(&self) -> bool {
        self.used_injected_plan.load(Ordering::SeqCst)
    }

    pub fn evidence_root(&self) -> &Path {
        self._tmp.path()
    }
}

pub fn forge_goal() -> String {
    r#"Goal: At the repository root, create a single new file named `FORGE_E2E.md` whose entire contents are exactly the single line `forge-e2e-ok`. Do not modify any other file, do not run `git push`, and do not open a pull request.
Acceptance: `FORGE_E2E.md` exists at the repository root with exactly that content."#
        .to_string()
}

pub fn forge_pr_goal(repo_gitdir: &str) -> String {
    format!(
        r#"Goal: In your current leased git worktree, perform EXACTLY these steps in order, using the MCP tools provided to you. Do not use the shell for git/gh; use the MCP tools.

1. Create a single new file named `FORGE_E2E.md` at the worktree root whose entire contents are exactly the single line `forge-e2e-ok`. Do not modify any other file.

2. Call the MCP tool whose name ends in `git.commit` (full name `plugin.dev.neige.git-forge_git.commit`) with arguments {{"message":"forge-e2e worker commit","idem":"forge-e2e-worker-commit"}} to commit that file on your current slice branch. Note the `branch` it reports.

3. Call the MCP tool whose name ends in `gh.pr.create` (full name `plugin.dev.neige.git-forge_gh.pr.create`) with arguments:
   {{"repo":"{repo}","head":"<the branch git.commit reported in step 2>","base":"main","title":"forge-e2e","body":"forge-e2e worker PR"}}
   The `repo` value MUST be exactly `{repo}`. ALL of repo, head, base, title, body are mandatory. Note the PR `number` it returns.

4. Call the MCP tool whose name ends in `gh.pr.checks` (full name `plugin.dev.neige.git-forge_gh.pr.checks`) with arguments {{"repo":"{repo}","pr":<the PR number from step 3>}}.

5. Call `neige.task.complete` with a non-empty `idempotency_key`.

Hard constraints: Do NOT run `git push`. Do NOT call gh.pr.merge. Do NOT close any issue. Do NOT call gh.pr.diff or gh.pr.list. Perform steps 2, 3, 4 in that exact order BEFORE calling neige.task.complete in step 5.
Acceptance: `FORGE_E2E.md` exists with exactly that content; a PR was created; its checks were read."#,
        repo = repo_gitdir
    )
}

pub async fn worker_operation_for_task(repo: &SqlxRepo, task_id: &str) -> Option<OperationRow> {
    operation_for_idem(repo, "codex-worker", task_id).await
}
