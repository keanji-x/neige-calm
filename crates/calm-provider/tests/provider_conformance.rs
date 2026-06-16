#[cfg(all(unix, feature = "codex-e2e"))]
use std::path::PathBuf;
#[cfg(all(unix, feature = "codex-e2e"))]
use std::sync::Arc;

#[cfg(unix)]
use calm_provider::{ClaudeProvider, TerminalProvider};
#[cfg(all(unix, feature = "codex-e2e"))]
use calm_provider::{CodexDaemonProbe, CodexProvider};

#[cfg(unix)]
#[tokio::test]
async fn terminal_provider_conformance_unknown_without_supervisor() {
    let temp = tempfile::tempdir().expect("tempdir");
    let sock = temp.path().join("missing-proc-supervisor.sock");
    calm_truth_test_harness::provider_conformance(TerminalProvider::new(sock)).await;
}

#[cfg(unix)]
#[tokio::test]
async fn claude_provider_conformance_unknown_without_supervisor() {
    let temp = tempfile::tempdir().expect("tempdir");
    let sock = temp.path().join("missing-proc-supervisor.sock");
    calm_truth_test_harness::provider_conformance(ClaudeProvider::new(sock)).await;
}

#[cfg(all(unix, feature = "codex-e2e"))]
#[tokio::test]
async fn codex_provider_conformance_unknown_without_supervisor() {
    let Some(_codex_bin) = resolve_codex_bin() else {
        let raw =
            std::env::var("NEIGE_CODEX_BIN").unwrap_or_else(|_| DEFAULT_CODEX_BIN.to_string());
        eprintln!(
            "[codex-e2e] codex not found at {raw}; skipping (set NEIGE_CODEX_BIN to override)",
        );
        return;
    };

    let temp = tempfile::tempdir().expect("tempdir");
    let sock = temp.path().join("missing-proc-supervisor.sock");
    let daemon = Arc::new(StaticCodexDaemonProbe {
        running: false,
        active_turn_id: None,
        remote_uri: "ws://x".into(),
    });
    calm_truth_test_harness::provider_conformance(CodexProvider::new(sock, daemon)).await;
}

#[cfg(all(unix, feature = "codex-e2e"))]
const DEFAULT_CODEX_BIN: &str = "~/.nvm/versions/node/v24.4.1/bin/codex";

#[cfg(all(unix, feature = "codex-e2e"))]
fn resolve_codex_bin() -> Option<PathBuf> {
    let raw = std::env::var("NEIGE_CODEX_BIN").unwrap_or_else(|_| DEFAULT_CODEX_BIN.to_string());
    let expanded = if let Some(stripped) = raw.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        PathBuf::from(home).join(stripped)
    } else {
        PathBuf::from(raw)
    };
    if !expanded.is_file() {
        return None;
    }
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(&expanded).ok()?;
    if meta.permissions().mode() & 0o111 == 0 {
        return None;
    }
    Some(expanded)
}

#[cfg(all(unix, feature = "codex-e2e"))]
struct StaticCodexDaemonProbe {
    running: bool,
    active_turn_id: Option<String>,
    remote_uri: String,
}

#[cfg(all(unix, feature = "codex-e2e"))]
#[async_trait::async_trait]
impl CodexDaemonProbe for StaticCodexDaemonProbe {
    fn is_running(&self) -> bool {
        self.running
    }

    fn active_turn_id_for_thread(&self, _thread_id: &str) -> Option<String> {
        self.active_turn_id.clone()
    }

    fn remote_uri(&self) -> String {
        self.remote_uri.clone()
    }

    async fn read_liveness_facts(
        &self,
        _thread_id: &str,
    ) -> Option<calm_provider::CodexLivenessFacts> {
        None
    }
}
