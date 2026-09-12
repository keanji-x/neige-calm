use calm_server::dedicated_codex::*;
use calm_worker_runtime::{BoundaryState, Runtime, RuntimeConfig};
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

mod admission_tests;
mod bootstrap_tests;
mod cancellation_tests;
mod fake;
mod policy_tests;
mod review_tests;
mod session_tests;

struct Fixture {
    root: tempfile::TempDir,
    controller: Controller,
    runtime_config: RuntimeConfig,
    config: ControllerConfig,
    seed: HomeSeed,
    native: NativeMcp,
    _mcp_socket: std::os::unix::net::UnixListener,
    endpoints: Mutex<Vec<PreparedEndpoint>>,
}

impl Fixture {
    fn new(scenario: &str) -> Self {
        Self::with_config(scenario, |_| {})
    }

    fn with_config(scenario: &str, configure: impl FnOnce(&mut ControllerConfig)) -> Self {
        let root = tempfile::Builder::new()
            .prefix("neige-dc-")
            .tempdir()
            .unwrap();
        let executable = std::env::current_exe().unwrap();
        let helper = executable
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("calm-worker-boundary");
        assert!(
            helper.is_file(),
            "build calm-worker-runtime --bin calm-worker-boundary in the SAME target before server tests"
        );
        let runtime_config = RuntimeConfig {
            state_root: root.path().join("runtime"),
            helper,
            bwrap: "/usr/bin/bwrap".into(),
            timeout: Duration::from_secs(5),
        };
        let sandbox_bwrap = root.path().join("sandbox-bwrap");
        std::fs::write(
            &sandbox_bwrap,
            "#!/bin/sh\nprintf '%s\\n' '--argv0 --perms --ro-bind --unshare-user --unshare-net'\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sandbox_bwrap, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut config = ControllerConfig {
            private_root: root.path().join("endpoints"),
            runtime: runtime_config.clone(),
            codex_binary: executable.clone(),
            code_mode_host_binary: executable,
            mcp_shim: "/usr/bin/true".into(),
            sandbox_bwrap,
            provider_environment: BTreeMap::new(),
            connect_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_millis(500),
        };
        configure(&mut config);
        let controller = Controller::new(config.clone())
            .unwrap()
            .with_fixture_arguments(vec![
                "--ignored".into(),
                "--exact".into(),
                "dedicated_codex_cases::fake::fake_provider_process".into(),
                "--nocapture".into(),
                "--test-threads=1".into(),
            ]);
        let auth = root.path().join("source-auth.json");
        std::fs::write(&auth, r#"{"tokens":{"access_token":"AUTH_SECRET"}}"#).unwrap();
        let source = root.path().join("source-config.toml");
        std::fs::write(&source, format!("model = {scenario:?}\n[features]\ncode_mode=true\n[mcp_servers.untrusted]\ncommand='/evil'\n[hooks]\ncommand='/evil-hook'\n")).unwrap();
        let seed = HomeSeed::read(&source, &auth).unwrap();
        let socket = root.path().join("kernel.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let native = NativeMcp {
            socket,
            card_token: "MCP_SECRET".into(),
            plugin_tools: Vec::new(),
        };
        Self {
            root,
            controller,
            runtime_config,
            config,
            seed,
            native,
            _mcp_socket: listener,
            endpoints: Mutex::new(vec![]),
        }
    }

    async fn prepare(&self, run: &str) -> PreparedEndpoint {
        let workspace = self.root.path().join(format!("work-{run}"));
        std::fs::create_dir(&workspace).unwrap();
        // Caller preparation, before any baseline or Controller side effects.
        std::fs::create_dir(workspace.join(".codex")).unwrap();
        let request = DedicatedRequest {
            identity: DedicatedIdentity {
                run_id: run.into(),
                attempt_id: format!("attempt-{run}"),
                card_id: format!("card-{run}"),
                session_id: format!("session-{run}"),
            },
            workspace,
            developer_instructions: "unchanged frozen worker context".into(),
        };
        let endpoint = self
            .controller
            .prepare(request, &self.seed, &self.native)
            .await
            .unwrap();
        self.endpoints.lock().unwrap().push(endpoint.clone());
        endpoint
    }

    fn calls(endpoint: &PreparedEndpoint) -> Vec<serde_json::Value> {
        let text = std::fs::read_to_string(endpoint.home.home.join("fake-calls.jsonl"))
            .unwrap_or_default();
        text.lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Ok(runtime) = Runtime::new(self.runtime_config.clone()) {
            for endpoint in self.endpoints.get_mut().unwrap().iter() {
                let _ = runtime.stop(&endpoint.boundary, Duration::from_secs(3));
            }
        }
    }
}

#[derive(Default)]
struct Journal {
    states: Mutex<Vec<SessionRecord>>,
    current: Mutex<BTreeMap<String, SessionRecord>>,
    inside_admission: AtomicBool,
    reject: Mutex<Option<&'static str>>,
}

impl Journal {
    /// The test caller explicitly stores the prepared Operation before activation.
    fn prepared(&self, endpoint: PreparedEndpoint) -> SessionRecord {
        let record = SessionRecord::prepared(endpoint);
        assert!(
            self.current
                .lock()
                .unwrap()
                .insert(
                    record.endpoint.request.identity.run_id.clone(),
                    record.clone()
                )
                .is_none()
        );
        record
    }
}

#[async_trait::async_trait]
impl Checkpoint for Journal {
    async fn save(&self, expected: &SessionRecord, record: &SessionRecord) -> Result<()> {
        assert!(
            !self.inside_admission.load(Ordering::SeqCst),
            "checkpoint nested inside admission writer"
        );
        let mut current = self.current.lock().unwrap();
        let run = &record.endpoint.request.identity.run_id;
        if current.get(run) != Some(expected) {
            return Err(Error::Conflict("stale Operation checkpoint".into()));
        }
        let rejected = match *self.reject.lock().unwrap() {
            Some("thread") => matches!(record.phase, RequestPhase::CreatingThread),
            Some("turn") => matches!(record.phase, RequestPhase::IssuingTurn { .. }),
            Some("thread-ack") => matches!(record.phase, RequestPhase::ThreadReady { .. }),
            Some("turn-ack") => matches!(record.phase, RequestPhase::TurnActive { .. }),
            Some("start") => matches!(record.phase, RequestPhase::ProviderStarting),
            Some("stop") => matches!(record.stop, StopState::Requested),
            _ => false,
        };
        if rejected {
            return Err(Error::Conflict("operation checkpoint rejected".into()));
        }
        current.insert(run.clone(), record.clone());
        self.states.lock().unwrap().push(record.clone());
        Ok(())
    }
}

struct Allow;

#[async_trait::async_trait]
impl TurnAdmission for Allow {
    async fn admit(
        &self,
        launch: TurnLaunch,
    ) -> Result<calm_server::codex_appserver::TurnStartResult> {
        launch.issue().await
    }
}
