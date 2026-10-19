//! Agent identity, minimum version, authentication and agent options through the production
//! registration read, readiness, catalog and turn launch, against the spy peer. The profiles that
//! authenticate or read `_meta` are this module's own: no registered provider declares one yet.
#![cfg(test)]
use super::*;
use crate::acp_planner::config::{AcpPlannerConfig, AcpPlannerHost};
use crate::acp_planner::process::{LaunchContext, Process};
use crate::acp_planner::profile::{AcpAgentProfile, AgentEntry, AgentIdentity, ModeLaunch};
use crate::acp_planner::session::{AcpPlannerSession, SessionParams};
use crate::agent_providers::{Freshness, ProviderAvailabilityCache, Verdict};
use crate::planner_permission_mode::PlannerPermissionMode;
use provider::control::{CorrelationKey, InputRequest, TurnEdit, TurnRequest, TurnStartOutcome};
use serde_json::Value;
use std::sync::Arc;

const PEER: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/acp_planner/agent.py"
);
type Profiles = fn(&AgentProvider) -> Option<&'static AcpAgentProfile>;

/// Grok Build's entry shape: `agent <options> --no-leader stdio`.
static AUTHENTICATING: AcpAgentProfile = AcpAgentProfile {
    entry: AgentEntry {
        before_options: &["agent"],
        after_options: &["--no-leader", "stdio"],
    },
    identity: AgentIdentity::AgentInfo { name: "OpenCode" },
    authentication: Some("cached_token"),
    ask: ModeLaunch {
        env: &[],
        agent_options: &["--ask-first"],
    },
    full: ModeLaunch {
        env: &[],
        agent_options: &["--always-approve", "--quiet"],
    },
};
static META_VERSION: AcpAgentProfile = AcpAgentProfile {
    entry: AgentEntry {
        before_options: &[],
        after_options: &[],
    },
    identity: AgentIdentity::MetaAgentVersion,
    authentication: None,
    ask: ModeLaunch {
        env: &[],
        agent_options: &[],
    },
    full: ModeLaunch {
        env: &[],
        agent_options: &[],
    },
};
fn authenticating(_: &AgentProvider) -> Option<&'static AcpAgentProfile> {
    Some(&AUTHENTICATING)
}
fn meta_version(_: &AgentProvider) -> Option<&'static AcpAgentProfile> {
    Some(&META_VERSION)
}

/// A host reading a real registration file for the spy peer, with `env` added to the peer's
/// registered environment; `profiles` replaces the production profiles when given.
fn host(root: &Path, env: Value, min: &str, profiles: Option<Profiles>) -> AcpPlannerHost {
    std::fs::write(root.join("scenario"), "reply").unwrap();
    let mut env = env;
    env["ACP_FIXTURE_ROOT"] = json!(root);
    let path = root.join("acp.json");
    std::fs::write(
        &path,
        json!({"agents":[{"provider":"opencode","command":"/usr/bin/python3",
            "args":["-u",PEER,"prefix"],"env":env,"min_agent_version":min}]})
        .to_string(),
    )
    .unwrap();
    let host = AcpPlannerHost::new(
        Some(AcpPlannerConfig::read(&path).unwrap()),
        root,
        root.join("unused-shim"),
        root.join("mcp.sock"),
    )
    .unwrap();
    match profiles {
        Some(profiles) => host.with_profiles(profiles),
        None => host,
    }
}

fn lines(root: &Path, file: &str) -> Vec<Value> {
    std::fs::read_to_string(root.join(file))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
fn methods(root: &Path) -> Vec<String> {
    lines(root, "requests.jsonl")
        .iter()
        .filter_map(|frame| frame["method"].as_str().map(str::to_owned))
        .collect()
}
/// Every peer process that answered a frame has exited and been reaped.
fn all_stopped(root: &Path) -> bool {
    lines(root, "requests.jsonl")
        .iter()
        .all(|frame| !Path::new(&format!("/proc/{}", frame["_pid"])).exists())
}

async fn verdict(host: &AcpPlannerHost) -> Verdict {
    ProviderAvailabilityCache::default()
        .acp(&AgentProvider::OpenCode, Freshness::Recheck, host)
        .await
        .verdict
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_declared_method_authenticates_after_initialize_and_before_session_new() {
    let root = tempfile::tempdir().unwrap();
    let host = host(
        root.path(),
        json!({"ACP_FIXTURE_AUTH_METHODS":"browser,cached_token"}),
        "1.0.0",
        Some(authenticating),
    );
    assert_eq!(verdict(&host).await, Verdict::Ready);
    let catalog = host.discover_catalog(&AgentProvider::OpenCode, None).await;
    assert!(!catalog.models.is_empty(), "{catalog:?}");
    assert_eq!(
        methods(root.path()),
        [
            "initialize",
            "authenticate",
            "initialize",
            "authenticate",
            "session/new",
            "session/close"
        ]
    );
    for frame in lines(root.path(), "requests.jsonl") {
        if frame["method"] == "authenticate" {
            assert_eq!(frame["params"], json!({"methodId":"cached_token"}));
        }
    }
    assert!(all_stopped(root.path()));
}

/// OpenCode's profile names no method: an agent offering some still gets none, and its frames
/// are what they were before authentication existed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opencode_sends_no_authenticate_even_when_the_agent_offers_methods() {
    let root = tempfile::tempdir().unwrap();
    let host = host(
        root.path(),
        json!({"ACP_FIXTURE_AUTH_METHODS":"opencode-login"}),
        "1.0.0",
        None,
    );
    assert_eq!(verdict(&host).await, Verdict::Ready);
    host.discover_catalog(&AgentProvider::OpenCode, None).await;
    assert_eq!(
        methods(root.path()),
        ["initialize", "initialize", "session/new", "session/close"]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unoffered_refused_or_unanswered_authentication_leaves_the_provider_unavailable() {
    // The catalog launch takes the same path; it is driven once, so the silent case waits out
    // the authentication bound once.
    for (env, cause, sent, catalog) in [
        (
            json!({"ACP_FIXTURE_AUTH_METHODS":"browser"}),
            "does not offer authentication method `cached_token`",
            vec!["initialize"],
            false,
        ),
        (
            json!({"ACP_FIXTURE_AUTH_METHODS":"cached_token","ACP_FIXTURE_AUTHENTICATE":"error"}),
            "RPC error -32000",
            vec!["initialize", "authenticate"],
            true,
        ),
        (
            json!({"ACP_FIXTURE_AUTH_METHODS":"cached_token","ACP_FIXTURE_AUTHENTICATE":"silent"}),
            "timed out",
            vec!["initialize", "authenticate"],
            false,
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let host = host(root.path(), env, "1.0.0", Some(authenticating));
        let Verdict::Unavailable(reason) = verdict(&host).await else {
            panic!("{cause}: the provider must read unavailable");
        };
        assert!(
            reason.contains(cause)
                && reason.contains("sign it in with its own CLI under the environment"),
            "{reason}"
        );
        assert_eq!(methods(root.path()), sent, "{cause}");
        assert!(all_stopped(root.path()), "{cause}");
        let environment = lines(root.path(), "environment.jsonl");
        assert_eq!(environment.len(), 1);
        assert_eq!(
            environment[0]["NEIGE_MCP_TOKEN"], false,
            "readiness holds no token"
        );
        if catalog {
            let catalog = host.discover_catalog(&AgentProvider::OpenCode, None).await;
            assert!(catalog.models.is_empty(), "{catalog:?}");
            assert!(!methods(root.path()).contains(&"session/new".to_owned()));
            assert!(all_stopped(root.path()), "{cause}");
        }
    }
}

/// A turn's launch issues its card context before spawning; a failed authentication stops the
/// process before any session operation and revokes that context.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_authentication_revokes_the_turns_issued_context() {
    let root = tempfile::tempdir().unwrap();
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let row = owner(&repo, root.path(), AgentProvider::OpenCode).await;
    let host = Arc::new(host(
        root.path(),
        json!({"ACP_FIXTURE_AUTH_METHODS":"cached_token","ACP_FIXTURE_AUTHENTICATE":"error"}),
        "1.0.0",
        Some(authenticating),
    ));
    let session = AcpPlannerSession::open(SessionParams {
        host,
        provider: AgentProvider::OpenCode,
        repo: repo.clone(),
        seals: Arc::new(crate::thread_seals::ThreadSeals::new()),
        worker_session_id: row.worker.clone(),
        card_id: row.card.clone(),
        track_id: row.track.clone(),
        cwd: root.path().into(),
        instructions: String::new(),
    });
    let (held, _receiver) = crate::harness::held_requests::channel();
    session.mark_installed(held);
    let outcome = session
        .start_turn(
            &row.thread,
            TurnRequest {
                input: InputRequest {
                    items: vec![provider::InputItem::text("signed out")],
                    correlation: CorrelationKey("signed-out".into()),
                },
                selection: provider::TurnModelSelection::inherit(),
                permission: PlannerPermissionMode::Never,
                edit: TurnEdit::NoEdit,
            },
            &[crate::harness::QueueEntry::user_message(
                "signed out".into(),
                None,
                vec![],
            )],
        )
        .await
        .unwrap();
    assert!(
        matches!(&outcome, TurnStartOutcome::Failed { error } if error.to_string().contains("sign it in")),
        "{outcome:?}"
    );
    let environment = lines(root.path(), "environment.jsonl");
    assert_eq!(
        environment.last().unwrap()["NEIGE_MCP_TOKEN"],
        true,
        "the failure must exercise an issued context"
    );
    assert_eq!(token(&repo, &row).await, None);
    assert_eq!(methods(root.path()), ["initialize", "authenticate"]);
    assert!(all_stopped(root.path()));
    session.shutdown().await.unwrap();
}

/// argv is the registration's `args`, then the profile's entry with the turn mode's agent options
/// inside it. OpenCode's is `acp` under every mode and outside a turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_profiles_entry_wraps_the_turns_agent_options_after_the_registered_args() {
    let planner = |permission| LaunchContext::Planner {
        mcp_token: "card-context",
        permission,
    };
    for (profiles, context, argv) in [
        (
            Some(authenticating as Profiles),
            LaunchContext::Readiness,
            vec!["prefix", "agent", "--no-leader", "stdio"],
        ),
        (
            Some(authenticating),
            planner(PlannerPermissionMode::Never),
            vec!["prefix", "agent", "--no-leader", "stdio"],
        ),
        (
            Some(authenticating),
            planner(PlannerPermissionMode::Ask),
            vec!["prefix", "agent", "--ask-first", "--no-leader", "stdio"],
        ),
        (
            Some(authenticating),
            planner(PlannerPermissionMode::Full),
            vec![
                "prefix",
                "agent",
                "--always-approve",
                "--quiet",
                "--no-leader",
                "stdio",
            ],
        ),
        (None, LaunchContext::Readiness, vec!["prefix", "acp"]),
        (
            None,
            planner(PlannerPermissionMode::Never),
            vec!["prefix", "acp"],
        ),
        (
            None,
            planner(PlannerPermissionMode::Ask),
            vec!["prefix", "acp"],
        ),
        (
            None,
            planner(PlannerPermissionMode::Full),
            vec!["prefix", "acp"],
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let host = host(
            root.path(),
            json!({"ACP_FIXTURE_AUTH_METHODS":"cached_token"}),
            "1.0.0",
            profiles,
        );
        let registration = host.registration(&AgentProvider::OpenCode).unwrap();
        let process = Process::spawn(&host, registration, "options", root.path(), context)
            .await
            .unwrap();
        process.stop(&host, "options").await.unwrap();
        assert_eq!(lines(root.path(), "argv.jsonl"), [json!(argv)]);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_reported_version_is_admitted_from_the_registered_minimum() {
    for (env, min, profiles, admitted) in [
        (json!({}), "1.18.35", None, true),
        (json!({}), "1.18.34", None, true),
        (json!({}), "1.18.36", None, false),
        (json!({"ACP_FIXTURE_VERSION":"1.18"}), "1.0.0", None, false),
        (
            json!({"ACP_FIXTURE_IDENTITY":"meta","ACP_FIXTURE_VERSION":"1.0.62"}),
            "1.0.62",
            Some(meta_version as Profiles),
            true,
        ),
        (
            json!({"ACP_FIXTURE_IDENTITY":"meta","ACP_FIXTURE_VERSION":"1.0.61"}),
            "1.0.62",
            Some(meta_version as Profiles),
            false,
        ),
        // OpenCode's profile reads only `agentInfo`.
        (
            json!({"ACP_FIXTURE_IDENTITY":"meta","ACP_FIXTURE_VERSION":"1.18.35"}),
            "1.0.0",
            None,
            false,
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let host = host(root.path(), env.clone(), min, profiles);
        let ready = host.check_ready(&AgentProvider::OpenCode).await;
        assert_eq!(ready.is_ok(), admitted, "{env} at minimum {min}: {ready:?}");
        if let Err(error) = ready {
            assert!(
                error
                    .to_string()
                    .contains(&format!("version {min} or later")),
                "{error}"
            );
        }
        assert_eq!(methods(root.path()), ["initialize"]);
        assert!(all_stopped(root.path()));
    }
}
