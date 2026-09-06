//! Explicit fixture-only, zero-model command probe through the existing RPC client.
use super::*;
use std::collections::BTreeMap;

impl CodexAppServer {
    pub(crate) async fn command_exec_for_fixture(
        &self,
        command: Vec<String>,
        environment: BTreeMap<String, String>,
    ) -> Result<Value> {
        self.request(
            "command/exec",
            json!({
                "command": command,
                "cwd": "/workspace",
                "permissionProfile": crate::dedicated_codex::DELIVERY_PROFILE,
                "env": environment,
                "timeoutMs": 10_000,
                "outputBytesCap": 32_768,
            }),
        )
        .await
    }
}
