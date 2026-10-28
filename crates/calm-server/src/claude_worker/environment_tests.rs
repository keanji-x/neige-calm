//! The worker's environment and launch contract (§3.4.1 as revised): the supervisor's allowlist,
//! the kernel's keys, and the Held payload as the only carrier of the token and secret proxies.
use super::{
    WorkerArgs, WorkerEnvironment, WorkerLaunch, WorkerMcpToken, WorkerStart, kernel_proxy,
};
use crate::claude_planner::role::{ConfigSource, PlannerEnvironment};
use crate::claude_planner::spawn::{EnvInputs, base_env, passthrough_keys};
use crate::error::Result;
use crate::execution_environment::PROXY_KEYS;
use crate::mcp_server::wiring::card_mcp_env;
use crate::operation::claude_adapter::claude_card_env;
use crate::provider_execution as execution;
use calm_session::control::execution::{AmbientEnvironment, AmbientProxyPolicy};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::Path;

fn worker_env<'a>(
    config_source: ConfigSource<'a>,
    proxy: Vec<(String, OsString)>,
) -> WorkerEnvironment<'a> {
    WorkerEnvironment {
        config_source,
        path: "/opt/neige/bin:/usr/bin".into(),
        card_id: "card-1",
        base_url: "http://127.0.0.1:7777",
        mcp_socket: Path::new("/run/neige/mcp.sock"),
        proxy,
    }
}

fn every_proxy() -> Vec<(String, OsString)> {
    kernel_proxy(Some("http://settings.example:3128"), None, |key| {
        Some(match key {
            "NO_PROXY" | "no_proxy" => "localhost,.internal".into(),
            _ => "http://kernel.example:8080".into(),
        })
    })
}

/// The keys the worker's process starts with, the token aside: what the supervisor merges for the
/// declared ambient environment, by its own table, and the kernel's keys.
fn start_keys(env: &WorkerEnvironment<'_>) -> BTreeSet<String> {
    let mut keys: BTreeSet<String> = match WorkerEnvironment::ambient() {
        AmbientEnvironment::SupervisorAllowed { proxy_policy } => {
            calm_proc_supervisor::ambient_keys(&proxy_policy)
                .map(String::from)
                .collect()
        }
        AmbientEnvironment::None => BTreeSet::new(),
    };
    keys.extend(env.kernel_env().into_iter().map(|(key, _)| key));
    keys
}

#[test]
fn w_claude_worker_env_uses_user_config() {
    let env = worker_env(ConfigSource::UserHome, Vec::new());
    assert!(
        !env.kernel_env()
            .iter()
            .any(|(key, _)| key == "CLAUDE_CONFIG_DIR")
    );
    let keys = start_keys(&env);
    assert!(!keys.contains("CLAUDE_CONFIG_DIR"));
    // HOME carries the owner's login and settings (WA0 E9); the rest are the Legacy worker's keys
    // the Planner's allowlist lacks (§3.4.1).
    for key in [
        "HOME",
        "USER",
        "PATH",
        "SSH_AUTH_SOCK",
        "XDG_CONFIG_HOME",
        "DISPLAY",
        "SSL_CERT_DIR",
        "CURL_CA_BUNDLE",
        "NODE_EXTRA_CA_CERTS",
    ] {
        assert!(keys.contains(key), "{key}");
    }
}

#[test]
fn w_claude_worker_env_matches_legacy_allowlist() {
    let env = worker_env(ConfigSource::UserHome, every_proxy());
    // The Legacy PTY spawn: the supervisor's whole table, the card's hook keys and its MCP pair.
    let mut legacy: BTreeSet<String> =
        calm_proc_supervisor::ambient_keys(&AmbientProxyPolicy::RejectSecret)
            .map(String::from)
            .collect();
    legacy.extend(
        claude_card_env("card-1", "http://127.0.0.1:7777")
            .into_iter()
            .chain(card_mcp_env(Path::new("/run/neige/mcp.sock"), "token"))
            .map(|(key, _)| key.to_string())
            .filter(|key| key != "NEIGE_MCP_TOKEN"),
    );
    assert_eq!(start_keys(&env), legacy);
    // The supervisor stays the one source of its table: the kernel sets only PATH and the proxy
    // keys it resolves, and the supervisor merges no proxy of its own.
    let table: BTreeSet<&str> =
        calm_proc_supervisor::ambient_keys(&AmbientProxyPolicy::RejectSecret).collect();
    for (key, _) in env.kernel_env() {
        if table.contains(key.as_str()) {
            assert!(key == "PATH" || PROXY_KEYS.contains(&key.as_str()), "{key}");
        }
    }
    let AmbientEnvironment::SupervisorAllowed { proxy_policy } = WorkerEnvironment::ambient()
    else {
        panic!("the worker declares no supervisor ambient environment");
    };
    assert!(
        calm_proc_supervisor::ambient_keys(&proxy_policy).all(|key| !PROXY_KEYS.contains(&key))
    );
    let resolved: BTreeSet<String> = every_proxy().into_iter().map(|(key, _)| key).collect();
    assert_eq!(
        resolved,
        PROXY_KEYS.iter().map(|key| key.to_string()).collect()
    );
}

#[test]
fn w_claude_worker_dedicated_config_sets_dir() {
    let user = worker_env(ConfigSource::UserHome, Vec::new()).kernel_env();
    let dedicated = worker_env(
        ConfigSource::Dedicated(Path::new("/srv/neige/worker-claude")),
        Vec::new(),
    )
    .kernel_env();
    let added: Vec<_> = dedicated
        .iter()
        .filter(|entry| !user.contains(entry))
        .collect();
    assert_eq!(
        added,
        [&(
            "CLAUDE_CONFIG_DIR".to_string(),
            OsString::from("/srv/neige/worker-claude")
        )]
    );
    assert_eq!(dedicated.len(), user.len() + 1);
}

#[test]
fn w_claude_worker_kernel_proxy_prefers_settings() {
    let proxy = kernel_proxy(Some("http://settings.example:3128"), None, |key| {
        (key == "HTTP_PROXY" || key == "https_proxy").then(|| "http://kernel.example:8080".into())
    });
    assert_eq!(
        proxy,
        [
            ("HTTP_PROXY", "http://settings.example:3128"),
            ("http_proxy", "http://settings.example:3128"),
            ("HTTPS_PROXY", "http://kernel.example:8080"),
            ("https_proxy", "http://kernel.example:8080"),
        ]
        .map(|(key, value)| (key.to_string(), OsString::from(value)))
    );
}

#[test]
fn w_planner_env_unchanged() {
    let proxy = [(
        "HTTPS_PROXY".to_string(),
        "https_proxy".to_string(),
        "http://proxy.example:3128".to_string(),
    )];
    let env = base_env(
        &PlannerEnvironment,
        &EnvInputs {
            path: "/usr/bin".into(),
            config_source: ConfigSource::Dedicated(Path::new("/planner/config")),
            mcp_socket: Path::new("/run/mcp.sock"),
            proxy: &proxy,
        },
    );
    let mut expected: Vec<(String, OsString)> = passthrough_keys()
        .filter_map(|key| std::env::var_os(key).map(|value| (key.into(), value)))
        .collect();
    expected.extend([
        ("HTTPS_PROXY".into(), "http://proxy.example:3128".into()),
        ("https_proxy".into(), "http://proxy.example:3128".into()),
        ("PATH".into(), "/usr/bin".into()),
        ("CLAUDE_CONFIG_DIR".into(), "/planner/config".into()),
        ("NEIGE_MCP_SOCKET".into(), "/run/mcp.sock".into()),
        ("DISABLE_AUTOUPDATER".into(), "1".into()),
        ("CLAUDE_CODE_DISABLE_AUTO_MEMORY".into(), "1".into()),
    ]);
    assert_eq!(env, expected);
}

struct LaunchOwner;
#[async_trait::async_trait]
impl execution::ProviderExecutionOwner for LaunchOwner {
    fn consumer(&self) -> &'static str {
        "claude-worker"
    }
    fn alias_prefix(&self) -> execution::AliasPrefix {
        execution::AliasPrefix::planner()
    }
    fn alias(&self, session: &str, id: &str) -> String {
        self.alias_prefix().alias(session, id)
    }
    async fn complete(
        &self,
        _: &str,
        _: &execution::Record,
        _: execution::CompletionEvidence,
        _: Option<execution::KnownEnd>,
    ) -> Result<()> {
        unreachable!("a launch is never completed here")
    }
}

#[tokio::test]
async fn w_claude_worker_launch_has_no_token() {
    const TOKEN: &str = "worker-token-SENTINEL";
    const SECRET: &str = "http://user:proxy-SENTINEL@proxy.example:8080";
    const PUBLIC: &str = "http://proxy.example:3128";
    let dir = tempfile::tempdir().unwrap();
    let mut proxy = kernel_proxy(Some(PUBLIC), Some(SECRET), |_| None);
    proxy.push(("NO_PROXY".into(), "localhost".into()));
    let launch = WorkerLaunch {
        binary: Path::new("/usr/local/bin/claude"),
        args: WorkerArgs {
            start: WorkerStart::New(uuid::Uuid::new_v4()),
            settings: Path::new("/data/claude-settings/card-1/settings.json"),
            mcp_shim: Path::new("/opt/neige/neige-mcp-stdio-shim"),
            system_prompt: "You are a worker.",
        },
        environment: worker_env(ConfigSource::UserHome, proxy),
    };
    let id = workspace_runtime::execution::ExecutionId::fresh().unwrap();
    let (intent, payload) = launch
        .intent(
            &LaunchOwner,
            "worker-session-1",
            "track-1",
            dir.path(),
            &id,
            dir.path(),
            WorkerMcpToken::new(TOKEN.into()),
        )
        .await
        .unwrap();
    let specification = &intent.metadata.program;
    // What the Engine and supervisor journal, and the argv `/proc` shows.
    let durable = serde_json::to_string(&intent.metadata).unwrap();
    let payload = String::from_utf8(payload).unwrap();
    for secret in [TOKEN, "proxy-SENTINEL"] {
        assert!(!durable.contains(secret), "{secret} in the specification");
        assert!(
            !specification.args.iter().any(|arg| arg.contains(secret)),
            "{secret} in argv"
        );
        assert!(
            !specification
                .environment
                .iter()
                .any(|(_, value)| value.contains(secret)),
            "{secret} in the environment"
        );
        assert!(payload.contains(secret), "{secret} not released");
    }
    for key in ["NEIGE_MCP_TOKEN", "HTTPS_PROXY", "https_proxy"] {
        assert!(
            !specification.environment.iter().any(|(set, _)| set == key),
            "{key}"
        );
    }
    assert!(
        specification
            .environment
            .contains(&("HTTP_PROXY".into(), PUBLIC.into()))
    );
    assert!(
        specification
            .environment
            .contains(&("NO_PROXY".into(), "localhost".into()))
    );
    assert_eq!(
        specification.ambient_environment,
        WorkerEnvironment::ambient()
    );
    // The supervisor's own preflight accepts it: no secret proxy is explicit.
    assert_eq!(specification.validate_environment(), Ok(()));
}
