//! AGY's public statusLine callback. This opens no prompt and owns no MCP grant.
use super::{config::Registration, process};
use provider::acp::configuration::Configuration;
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use tokio::{io::AsyncReadExt, sync::watch};

const CAP: usize = 32 * 1024;
const BUDGET: Duration = Duration::from_secs(6);

#[derive(Clone, Debug)]
pub(super) struct Selection {
    model: String,
    effort: Option<String>,
    version: semver::Version,
}
impl Selection {
    pub fn freeze(
        registration: Registration<'_>,
        version: Option<&semver::Version>,
        config: &Configuration,
    ) -> Option<Self> {
        let version = version?;
        if !registration.profile.context_snapshot
            || version.major != 1
            || version < &semver::Version::new(1, 5, 3)
        {
            return None;
        }
        let model = config.category("model").filter(|option| {
            !option.current_value.is_empty() && option.accepts(&option.current_value)
        })?;
        let effort = match config.category("thought_level") {
            Some(option)
                if !option.current_value.is_empty() && option.accepts(&option.current_value) =>
            {
                Some(option.current_value.clone())
            }
            Some(_) => return None,
            None => None,
        };
        Some(Self {
            model: model.current_value.clone(),
            effort,
            version: version.clone(),
        })
    }
}

/// Native occupancy, never cumulative input/output billing totals.
fn decode(value: &Value, native: &str, selection: &Selection) -> Option<(i64, i64)> {
    if value["conversation_id"].as_str()? != native
        || value["version"].as_str()? != selection.version.to_string()
        || value["model"]["id"].as_str().is_none_or(str::is_empty)
        || !value["context_window"]["current_usage"].is_object()
    {
        return None;
    }
    if value["model"]["effort"].as_str() != selection.effort.as_deref() {
        return None;
    }
    let context = &value["context_window"];
    let size = context["context_window_size"]
        .as_i64()
        .filter(|size| *size > 0 && *size <= (1_i64 << 53))?;
    let used = context["used_percentage"]
        .as_f64()
        .filter(|v| v.is_finite() && (0.0..=100.0).contains(v))?;
    let remaining = context["remaining_percentage"]
        .as_f64()
        .filter(|v| v.is_finite() && (0.0..=100.0).contains(v))?;
    if (used + remaining - 100.0).abs() > 0.01 {
        return None;
    }
    Some(((size as f64 * used / 100.0).round() as i64, size))
}

#[cfg(unix)]
pub(super) async fn read(
    registration: Registration<'_>,
    cwd: &Path,
    native: &str,
    selection: &Selection,
    cancelled: &mut watch::Receiver<super::session::CancelRequest>,
    limit: &tokio::sync::Semaphore,
    admitted: impl Fn() -> bool,
) -> Option<(i64, i64)> {
    use plugin::child_process::{set_process_group_leader, spawn_tracked, terminal_input};
    use std::process::Stdio;
    let deadline = tokio::time::Instant::now() + BUDGET;
    let _permit = tokio::select! {
        biased;
        _ = cancelled.changed() => return None,
        result = tokio::time::timeout_at(deadline, limit.acquire()) => result.ok()?.ok()?,
    };
    // Waiting for another card's optional query must not resurrect an attachment closed meanwhile.
    if !admitted() {
        return None;
    }
    let scratch = tempfile::Builder::new()
        .prefix("neige-context-")
        .tempdir()
        .ok()?;
    let snapshot = tempfile::NamedTempFile::new_in(scratch.path()).ok()?;
    // The directory and file are Neige-owned; shell quoting does not reinterpret their bytes.
    let path = crate::shell_command::sh_single_quote(&snapshot.path().to_string_lossy());
    let callback = format!("head -c {} > {path}", CAP + 1);
    let settings =
        json!({"colorScheme":"terminal", "statusLine":{"type":"command","command":callback}});
    let mut command = tokio::process::Command::new(&registration.config.command);
    command
        .args(&registration.config.args)
        .arg(format!("--model={}", selection.model))
        .arg(format!("--conversation={native}"))
        .arg(format!("--settings={settings}"))
        .arg("--log-file=/dev/null")
        .current_dir(cwd)
        .env_clear()
        .envs(&registration.config.env)
        // Public CLI opt-out: reading context must not update the registered executable.
        // https://antigravity.google/docs/cli-troubleshooting
        .env("AGY_CLI_DISABLE_AUTO_UPDATE", "true")
        .env("PATH", crate::kernel_bin_path::kernel_led_path().ok()?.path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(effort) = &selection.effort {
        command.arg(format!("--effort={effort}"));
    }
    set_process_group_leader(&mut command);
    let _terminal = terminal_input(&mut command).ok()?;
    if tokio::time::Instant::now() >= deadline || !admitted() {
        return None;
    }
    let mut spawning = spawn_tracked(command);
    let spawned = tokio::select! {
        biased;
        _ = cancelled.changed() => None,
        _ = tokio::time::sleep_until(deadline) => None,
        result = &mut spawning => Some(result),
    };
    let abandoned = spawned.is_none();
    // A blocking fork cannot be cancelled safely. The original turn retains admission until
    // this exact attempt finishes and its late child is reaped, even after a query deadline.
    let mut child = match spawned {
        Some(result) => result,
        None => spawning.await,
    }
    .ok()?
    .ok()?;
    let reading = if abandoned {
        None
    } else {
        tokio::select! {
            biased;
            _ = cancelled.changed() => None,
            result = tokio::time::timeout_at(deadline, async {
                loop {
                if !admitted() { return None; }
                    let mut bytes = Vec::new();
                    let file = tokio::fs::File::open(snapshot.path()).await.ok()?;
                    file.take((CAP + 1) as u64).read_to_end(&mut bytes).await.ok()?;
                    if bytes.len() > CAP { return None; }
                    if let Ok(value) = serde_json::from_slice::<Value>(&bytes)
                        && let Some(reading) = decode(&value, native, selection) { return Some(reading); }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }) => result.ok().flatten(),
        }
    };
    // The retained turn owns this process until cleanup is confirmed. Optional telemetry cannot
    // turn a successful native result into Unknown, nor release admission with a live query.
    child.kill_group();
    while let Err(error) = process::reap(&mut child).await {
        tracing::warn!(%error, "Context query cleanup pending; retaining its turn fence");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    reading
}

#[cfg(not(unix))]
pub(super) async fn read(
    _: Registration<'_>,
    _: &Path,
    _: &str,
    _: &Selection,
    _: &mut watch::Receiver<super::session::CancelRequest>,
    _: &tokio::sync::Semaphore,
    _: impl Fn() -> bool,
) -> Option<(i64, i64)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    fn selection() -> Selection {
        Selection {
            model: "native-model".into(),
            effort: Some("low".into()),
            version: semver::Version::new(1, 5, 3),
        }
    }
    fn payload() -> Value {
        json!({"conversation_id":"bound","version":"1.5.3","model":{"id":"Native label","effort":"low"},
            "context_window":{"context_window_size":200000,"used_percentage":25.0,"remaining_percentage":75.0,
                "current_usage":{"input_tokens":9999999},"total_input_tokens":99999999}})
    }
    #[test]
    fn context_snapshot_reads_native_occupancy_and_rejects_foreign_or_unknown_readings() {
        let selection = selection();
        assert_eq!(
            decode(&payload(), "bound", &selection),
            Some((50000, 200000))
        );
        for (pointer, invalid) in [
            ("/conversation_id", json!("other")),
            ("/version", json!("1.5.4")),
            ("/model/effort", json!("high")),
            ("/model", Value::Null),
            ("/context_window/current_usage", Value::Null),
            ("/context_window/context_window_size", json!(0)),
            ("/context_window/used_percentage", json!(-1)),
            ("/context_window/used_percentage", json!(101)),
            ("/context_window/remaining_percentage", json!(50)),
        ] {
            let mut value = payload();
            *value.pointer_mut(pointer).unwrap() = invalid;
            assert_eq!(decode(&value, "bound", &selection), None, "{pointer}");
        }
        let mut fixed_effort = selection;
        fixed_effort.effort = None;
        assert_eq!(decode(&payload(), "bound", &fixed_effort), None);
        let mut value = payload();
        value["model"].as_object_mut().unwrap().remove("effort");
        assert_eq!(
            decode(&value, "bound", &fixed_effort),
            Some((50000, 200000))
        );
    }

    #[cfg(unix)]
    fn fixture(silent: bool) -> (tempfile::TempDir, super::super::config::AcpAgentConfig) {
        use super::super::config::AcpAgentConfig;
        use calm_types::runtime::AgentProvider;
        let root = tempfile::tempdir().unwrap();
        let script = root.path().join("agent.py");
        std::fs::write(root.path().join("payload.json"), payload().to_string()).unwrap();
        if silent {
            std::fs::write(root.path().join("silent"), "").unwrap();
        }
        std::fs::write(&script,r#"
import json,os,sys,pathlib,subprocess,time
root=pathlib.Path(sys.argv[1])
assert os.isatty(0)
assert 'NEIGE_MCP_TOKEN' not in os.environ
assert os.environ['AGY_CLI_DISABLE_AUTO_UPDATE']=='true'
args=dict(a[2:].split('=',1) for a in sys.argv[2:])
assert args['conversation']=='bound' and args['model']=='native-model' and args['effort']=='low'
helper=subprocess.Popen(['/bin/sleep','300'],stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
(root/'descendant').write_text(str(helper.pid))
(root/'pid').write_text(str(os.getpid()))
if not (root/'silent').exists():
    settings=json.loads(args['settings'])
    subprocess.run(settings['statusLine']['command'],input=(root/'payload.json').read_text(),text=True,shell=True,check=True)
while True:time.sleep(1)
"#).unwrap();
        let config = AcpAgentConfig {
            provider: AgentProvider::Antigravity,
            command: "/usr/bin/python3".into(),
            args: vec![
                script.display().to_string(),
                root.path().display().to_string(),
            ],
            env: std::collections::BTreeMap::from([(
                "AGY_CLI_DISABLE_AUTO_UPDATE".into(),
                "false".into(),
            )]),
            min_agent_version: semver::Version::new(1, 5, 0),
            default_permission_mode: None,
        };
        (root, config)
    }
    #[cfg(unix)]
    fn registration(config: &super::super::config::AcpAgentConfig) -> Registration<'_> {
        Registration {
            config,
            profile: super::super::profile::AcpAgentProfile::of(&config.provider).unwrap(),
        }
    }
    #[cfg(unix)]
    async fn assert_reaped(root: &Path) {
        let pid: i32 = std::fs::read_to_string(root.join("pid"))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None),
            Err(nix::errno::Errno::ESRCH)
        );
        #[cfg(target_os = "linux")]
        {
            let pid = std::fs::read_to_string(root.join("descendant")).unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let state = std::fs::read_to_string(format!("/proc/{pid}/stat"));
                    if state.is_err() || state.unwrap().split_once(") ").unwrap().1.starts_with('Z')
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("query descendants stopped before admission resumes");
        }
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn context_snapshot_runs_registered_cli_with_terminal_and_reaps_before_return() {
        let (root, config) = fixture(false);
        let (_cancel, mut cancelled) =
            watch::channel(super::super::session::CancelRequest::Running);
        let limit = tokio::sync::Semaphore::new(1);
        assert_eq!(
            read(
                registration(&config),
                root.path(),
                "bound",
                &selection(),
                &mut cancelled,
                &limit,
                || true
            )
            .await,
            Some((50000, 200000))
        );
        assert_reaped(root.path()).await;
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn context_snapshot_cancellation_reaps_query_and_descendants() {
        let (root, config) = fixture(true);
        let (cancel, mut cancelled) = watch::channel(super::super::session::CancelRequest::Running);
        let limit = tokio::sync::Semaphore::new(1);
        let selected = selection();
        let query = read(
            registration(&config),
            root.path(),
            "bound",
            &selected,
            &mut cancelled,
            &limit,
            || true,
        );
        let stop = async {
            tokio::time::timeout(Duration::from_secs(4), async {
                while !root.path().join("pid").exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            cancel
                .send(super::super::session::CancelRequest::Requested)
                .unwrap();
        };
        let (reading, ()) = tokio::join!(query, stop);
        assert_eq!(reading, None);
        assert_reaped(root.path()).await;
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn context_snapshot_rechecks_owner_after_waiting_for_query_capacity() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (root, config) = fixture(false);
        let (_cancel, mut cancelled) =
            watch::channel(super::super::session::CancelRequest::Running);
        let limit = tokio::sync::Semaphore::new(1);
        let permit = limit.acquire().await.unwrap();
        let live = AtomicBool::new(true);
        let selected = selection();
        let query = read(
            registration(&config),
            root.path(),
            "bound",
            &selected,
            &mut cancelled,
            &limit,
            || live.load(Ordering::Acquire),
        );
        let detach = async {
            tokio::task::yield_now().await;
            live.store(false, Ordering::Release);
            drop(permit);
        };
        let (reading, ()) = tokio::join!(query, detach);
        assert_eq!(reading, None);
        assert!(
            !root.path().join("pid").exists(),
            "a withdrawn owner must never start the reader"
        );
    }
}
