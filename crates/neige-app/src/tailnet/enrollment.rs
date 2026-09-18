use super::*;
use calm_types::enrollment::{EnrollmentAction, EnrollmentCommand, EnrollmentResult};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum HelperMode {
    Node,
    Cleanup,
    Status,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct CleanupReport {
    version: u32,
    pending_cleanup: u32,
    detail: String,
}

impl TailnetManager {
    pub(super) async fn cleanup_report(&self, mode: HelperMode) -> anyhow::Result<CleanupReport> {
        let mut child = self.spawn_mode(mode)?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Cleanup status channel unavailable"))?;
        let operation = async {
            let mut bytes = Vec::new();
            stdout.take(MAX_MESSAGE + 1).read_to_end(&mut bytes).await?;
            let exit = child.wait().await?;
            anyhow::ensure!(
                exit.success(),
                "Cleanup helper failed; pending cloud cleanup remains unconfirmed. Check private files and helper version."
            );
            anyhow::ensure!(
                bytes.len() <= MAX_MESSAGE as usize,
                "Cleanup status exceeds limit"
            );
            let report: CleanupReport = serde_json::from_slice(&bytes).map_err(|_| {
                anyhow::anyhow!("Cleanup status unavailable; update helper and fully restart Neige")
            })?;
            anyhow::ensure!(
                report.version == 2 && report.pending_cleanup <= 64,
                "Unsupported cleanup status; update helper and fully restart Neige"
            );
            Ok::<_, anyhow::Error>(report)
        };
        let result = tokio::time::timeout(
            Duration::from_secs(if mode == HelperMode::Status { 2 } else { 10 }),
            operation,
        )
        .await;
        match result {
            Ok(Ok(report)) => Ok(report),
            error => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                match error {
                    Ok(Err(e)) => Err(e),
                    _ => Err(anyhow::anyhow!(
                        "Cleanup helper timed out; cloud cleanup remains unconfirmed"
                    )),
                }
            }
        }
    }

    pub(super) async fn enrollment_action(
        &self,
        command: EnrollmentCommand,
    ) -> anyhow::Result<EnrollmentResult> {
        let state = self.state.lock().await;
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64;
        let id_valid = |s: &str| {
            !s.is_empty()
                && s.len() <= 128
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        };
        anyhow::ensure!(
            !state.shutdown
                && id_valid(&command.enrollment_id)
                && id_valid(&command.generation)
                && command.deadline > now
                && command.deadline <= now + 10_000,
            "Invalid or expired enrollment command"
        );
        if command.action == EnrollmentAction::Status
            && (!state.desired.desired_enabled || state.child.is_none())
        {
            let mut report = self.cleanup_report(HelperMode::Status).await?;
            if let Some(error) = &state.cleanup_error {
                report.detail = format!("{error}; {}", report.detail);
            }
            return Ok(EnrollmentResult {
                enrollment_id: command.enrollment_id,
                generation: command.generation,
                origin: String::new(),
                auth_key: String::new(),
                auth_key_expires_at: 0,
                pair_expires_at: 0,
                pending_cleanup: report.pending_cleanup,
                detail: report.detail,
            });
        }
        anyhow::ensure!(
            state.desired.desired_enabled && state.child.is_some(),
            "setup-required: enable private access before creating an enrollment"
        );
        TailnetClient::new(self.cfg.helper_socket())
            .enrollment(command)
            .await
    }
}
