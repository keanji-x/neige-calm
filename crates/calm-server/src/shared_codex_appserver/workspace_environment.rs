//! Install project environments at the real Codex execution boundary, not its viewer PTY.
use super::*;
use std::{ffi::OsStr, path::Path};

impl SharedCodexAppServer {
    pub(super) async fn workspace_thread_config(
        &self,
        client: &CodexAppServer,
        card_id: &str,
        cwd: Option<&Path>,
        config: &ThreadConfig,
        admission: &NativePromptAdmission<'_>,
    ) -> Result<Option<serde_json::Value>> {
        let selected;
        let cwd = match cwd {
            Some(cwd) => cwd,
            None => {
                selected = self.workspace_resume_cwd(card_id).await?;
                Path::new(&selected)
            }
        };
        let source = crate::workspace_service::environment::source_for_card(
            self.repo.as_ref(),
            card_id,
            cwd,
        )
        .await?;
        let native_cwd = cwd.to_str().ok_or_else(|| {
            CalmError::BadRequest("Codex workspace directory requires UTF-8".into())
        })?;
        let inherited = admission
            .finish(
                client
                    .config_read_gated(
                        Some(native_cwd),
                        tokio::time::Instant::now() + client.request_timeout(),
                        admission.gate(),
                    )
                    .await,
            )?
            .config
            .shell_environment_policy;
        let environment =
            crate::workspace_service::environment::resolve_for_directory_admitted(&source, || {
                admission.check_local()
            })
            .await?
            .map_err(crate::workspace_service::environment::map_environment_error)?;
        let path =
            crate::kernel_bin_path::kernel_led_path_from(environment.get(OsStr::new("PATH")))?;
        let mut set = serde_json::Map::new();
        for (key, value) in environment.into_values() {
            let key = key.into_string().map_err(|_| {
                CalmError::BadRequest("Codex workspace environment requires UTF-8 names".into())
            })?;
            let value = value.into_string().map_err(|_| {
                CalmError::BadRequest("Codex workspace environment requires UTF-8 values".into())
            })?;
            set.insert(key, serde_json::Value::String(value));
        }
        // Explicit owner settings win after project evaluation. Ambient values
        // remain subject to the project's overrides and unsets.
        for (upper, lower, value) in
            crate::proxy_env::configured_proxy_env_pairs(self.repo.as_ref()).await?
        {
            set.insert(upper.into(), serde_json::Value::String(value.clone()));
            set.insert(lower.into(), serde_json::Value::String(value));
        }
        let mut lowered = config
            .to_wire_config()?
            .unwrap_or_else(|| serde_json::json!({}));
        if let Some(kernel) = lowered
            .pointer("/shell_environment_policy/set")
            .and_then(serde_json::Value::as_object)
        {
            set.extend(
                kernel
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone())),
            );
        }
        set.insert(
            "PATH".into(),
            serde_json::Value::String(path.path_utf8()?.into()),
        );
        let requested_names = set.keys().cloned().collect::<Vec<_>>();
        let mut policy = provider::codex::shell_environment_filter_overrides(
            &requested_names,
            inherited.as_ref(),
        )
        .map_err(|error| CalmError::BadRequest(error.to_string()))?;
        policy.insert("inherit".into(), serde_json::json!("none"));
        policy.insert("experimental_use_profile".into(), serde_json::json!(false));
        policy.insert("set".into(), serde_json::Value::Object(set));
        lowered["shell_environment_policy"] = serde_json::Value::Object(policy);
        Ok(Some(lowered))
    }

    async fn workspace_resume_cwd(&self, card_id: &str) -> Result<String> {
        let card = self
            .repo
            .card_get(card_id)
            .await?
            .ok_or_else(|| CalmError::NotFound("workspace environment card".into()))?;
        let track = self
            .repo
            .track_get(card.track_id.as_str())
            .await?
            .ok_or_else(|| CalmError::NotFound("workspace environment track".into()))?;
        if card
            .payload
            .get(calm_truth::validation::BOUND_SESSION_CWD_PAYLOAD_KEY)
            .is_some()
            && let Some(runtime) = self
                .repo
                .session_projection_active_for_card(&card_id.to_owned())
                .await?
        {
            return crate::harness::wiring::session_cwd_for(
                self.repo.as_ref(),
                card_id,
                &runtime.id,
                &track,
            )
            .await;
        }
        if let Some(terminal) = self.repo.terminal_get_by_card(card_id).await? {
            return Ok(terminal.cwd);
        }
        Ok(track.workspace.agent_cwd().to_owned())
    }
}
