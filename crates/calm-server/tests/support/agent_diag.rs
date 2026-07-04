//! First-class agent-failure diagnostics for the real-codex E2E suite (#852).
//!
//! Any `wait_for_*` helper that can time out on real-agent behavior should
//! fail through [`panic_with_agent_diag`], which prints one combined dump:
//! operation phases/`last_error`, event-kind census + ordered event stream +
//! harness items, per-worktree `git status`/`git log`, every codex rollout
//! transcript under the fixture's CODEX_HOME (each `function_call` name +
//! `arguments` + output — the view that exposed the `git.commit`
//! `arguments:"{}"` bug in #850), and the shared appserver stderr.
//!
//! The fixture's temp dir is wrapped in [`EvidenceTempDir`], which leaks the
//! directory when the test thread panics (any panic, not just wait timeouts)
//! so the rollout transcripts and appserver stderr survive process exit for
//! post-mortem reads. On success the directory is removed as before.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use calm_server::db::sqlite::SqlxRepo;
use serde_json::Value;
use tempfile::TempDir;

use super::codex_fixture::{Fixture, read_lossy};
use super::git_helpers::run_git_output;

const SNIP_CHARS: usize = 2000;

pub struct EvidenceTempDir {
    dir: Option<TempDir>,
}

impl EvidenceTempDir {
    pub fn new(dir: TempDir) -> Self {
        Self { dir: Some(dir) }
    }

    pub fn path(&self) -> &Path {
        self.dir
            .as_ref()
            .expect("evidence tempdir taken before drop")
            .path()
    }
}

impl Drop for EvidenceTempDir {
    fn drop(&mut self) {
        if std::thread::panicking()
            && let Some(dir) = self.dir.take()
        {
            let path = dir.keep();
            eprintln!(
                "[agent-diag] test panicked: evidence preserved at {} \
                 (rollouts under codex-home*/**/sessions/, appserver stderr under logs/)",
                path.display()
            );
        }
    }
}

pub async fn panic_with_agent_diag(fx: &Fixture, reason: String) -> ! {
    let dump = agent_failure_dump(fx).await;
    panic!("{reason}\n{dump}");
}

pub async fn agent_failure_dump(fx: &Fixture) -> String {
    let mut out = String::from("\n==== AGENT FAILURE DIAG ====\n");
    let _ = writeln!(out, "-- evidence root (leaked on panic) --");
    let _ = writeln!(out, "  {}", fx.evidence_root().display());
    let _ = writeln!(out, "  appserver stderr: {}", fx.codex_stderr_log.display());
    out.push_str(&operations_diag(&fx.repo).await);
    out.push_str(&events_diag(&fx.repo).await);
    out.push_str(&worktree_diag(fx).await);
    out.push_str(&rollout_transcripts_diag(fx.evidence_root()));
    let _ = writeln!(out, "-- appserver stderr --");
    out.push_str(&read_lossy(&fx.codex_stderr_log));
    out.push_str("\n==== END AGENT FAILURE DIAG ====\n");
    out
}

async fn operations_diag(repo: &SqlxRepo) -> String {
    type OperationDiagRow = (String, String, Option<String>, String, Option<String>);
    let rows: Vec<OperationDiagRow> = sqlx::query_as(
        "SELECT id, kind, idempotency_key, phase, last_error \
         FROM operations ORDER BY created_at_ms ASC",
    )
    .fetch_all(repo.pool())
    .await
    .unwrap_or_default();
    let mut out = String::new();
    let _ = writeln!(out, "-- operations ({} rows) --", rows.len());
    for (id, kind, idem, phase, last_error) in &rows {
        let _ = writeln!(
            out,
            "  id={id} kind={kind} idem={idem:?} phase={phase} last_error={last_error:?}"
        );
    }
    out
}

/// `(id, turn_id, item_type, method, params)` for the harness-item diagnostic.
type HarnessItemDiagRow = (i64, Option<String>, Option<String>, String, String);

async fn events_diag(repo: &SqlxRepo) -> String {
    let mut out = String::new();

    let kinds: Vec<(String, i64)> =
        sqlx::query_as("SELECT kind, COUNT(*) FROM events GROUP BY kind ORDER BY kind")
            .fetch_all(repo.pool())
            .await
            .unwrap_or_default();
    let _ = writeln!(out, "-- event kind census --");
    for (k, n) in &kinds {
        let _ = writeln!(out, "  {k}: {n}");
    }

    let evs: Vec<(i64, String, String, String)> =
        sqlx::query_as("SELECT id, kind, actor, substr(payload,1,240) FROM events ORDER BY id ASC")
            .fetch_all(repo.pool())
            .await
            .unwrap_or_default();
    let _ = writeln!(out, "-- event stream ({} rows) --", evs.len());
    for (id, kind, actor, payload) in &evs {
        let _ = writeln!(out, "  #{id} {kind} actor={actor} {payload}");
    }

    let items: Vec<HarnessItemDiagRow> = sqlx::query_as(
        "SELECT id, turn_id, item_type, method, substr(params,1,360) \
         FROM harness_items ORDER BY id ASC",
    )
    .fetch_all(repo.pool())
    .await
    .unwrap_or_default();
    let _ = writeln!(
        out,
        "-- harness_items ({} rows: the spec's real turn) --",
        items.len()
    );
    for (id, turn, ty, method, params) in &items {
        let _ = writeln!(
            out,
            "  #{id} turn={turn:?} type={ty:?} method={method} {params}"
        );
    }
    out
}

async fn worktree_diag(fx: &Fixture) -> String {
    let mut cwds = vec![fx.wave_cwd.clone()];
    for cwd in operation_cwds(&fx.repo).await {
        if !cwds.contains(&cwd) {
            cwds.push(cwd);
        }
    }
    let mut out = String::new();
    for cwd in &cwds {
        let _ = writeln!(out, "-- worktree {} --", cwd.display());
        if !cwd.exists() {
            let _ = writeln!(out, "  <does not exist>");
            continue;
        }
        out.push_str(&git_section(
            cwd,
            "git status",
            ["status", "--short", "--branch", "--untracked-files=all"],
        ));
        out.push_str(&git_section(cwd, "git log", ["log", "--oneline", "-5"]));
    }
    out
}

async fn operation_cwds(repo: &SqlxRepo) -> Vec<PathBuf> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT tx_output_json FROM operations \
         WHERE tx_output_json IS NOT NULL ORDER BY created_at_ms ASC",
    )
    .fetch_all(repo.pool())
    .await
    .unwrap_or_default();
    rows.into_iter()
        .filter_map(|(raw,)| {
            let output: Value = serde_json::from_str(&raw).ok()?;
            Some(PathBuf::from(output["data"]["cwd"].as_str()?))
        })
        .collect()
}

fn git_section<const N: usize>(cwd: &Path, label: &str, args: [&str; N]) -> String {
    let output = run_git_output(Some(cwd), args);
    format!(
        "  {label}:\n{}{}",
        indent(&String::from_utf8_lossy(&output.stdout)),
        indent(&String::from_utf8_lossy(&output.stderr)),
    )
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|line| format!("    {line}\n"))
        .collect::<String>()
}

fn rollout_transcripts_diag(evidence_root: &Path) -> String {
    let files = find_rollout_files(evidence_root);
    let mut out = String::new();
    let _ = writeln!(
        out,
        "-- codex rollout transcripts ({} found, oldest first) --",
        files.len()
    );
    for path in &files {
        let _ = writeln!(out, "  == {} ==", path.display());
        out.push_str(&summarize_rollout_jsonl(&read_lossy(path)));
    }
    out
}

pub fn find_rollout_files(evidence_root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for home in ["codex-home", "codex-homes"] {
        collect_rollouts(&evidence_root.join(home), &mut found);
    }
    found.sort_by_key(|path| {
        std::fs::metadata(path)
            .and_then(|meta| meta.modified())
            .ok()
    });
    found
}

fn collect_rollouts(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rollouts(&path, found);
        } else if is_rollout_file(&path) {
            found.push(path);
        }
    }
}

fn is_rollout_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
}

/// Pure summarizer for one codex rollout `.jsonl` transcript: tool calls with
/// their raw `arguments` strings, their outputs, and notable turn events.
pub fn summarize_rollout_jsonl(content: &str) -> String {
    let mut out = String::new();
    for (idx, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
            let _ = writeln!(
                out,
                "  line {}: <unparseable json> {}",
                idx + 1,
                snip(trimmed)
            );
            continue;
        };
        let ts = value["timestamp"].as_str().unwrap_or("<no ts>");
        let payload = &value["payload"];
        match value["type"].as_str() {
            Some("session_meta") => {
                let _ = writeln!(
                    out,
                    "  [{ts}] session_meta id={} cwd={} originator={}",
                    payload["id"], payload["cwd"], payload["originator"]
                );
            }
            Some("response_item") => match payload["type"].as_str() {
                Some("function_call") => {
                    let _ = writeln!(
                        out,
                        "  [{ts}] function_call {} call_id={} arguments={}",
                        payload["name"],
                        payload["call_id"],
                        snip(payload["arguments"].as_str().unwrap_or("<non-string>"))
                    );
                }
                Some("custom_tool_call") => {
                    let _ = writeln!(
                        out,
                        "  [{ts}] custom_tool_call {} call_id={} input={}",
                        payload["name"],
                        payload["call_id"],
                        snip(payload["input"].as_str().unwrap_or("<non-string>"))
                    );
                }
                Some("function_call_output") | Some("custom_tool_call_output") => {
                    let _ = writeln!(
                        out,
                        "  [{ts}] -> output call_id={} {}",
                        payload["call_id"],
                        snip(&payload["output"].to_string())
                    );
                }
                Some("local_shell_call") => {
                    let _ = writeln!(
                        out,
                        "  [{ts}] local_shell_call {}",
                        snip(&payload["action"].to_string())
                    );
                }
                _ => {}
            },
            Some("event_msg") => match payload["type"].as_str() {
                Some("mcp_tool_call_begin") => {
                    let _ = writeln!(
                        out,
                        "  [{ts}] mcp_tool_call_begin {}",
                        snip(&payload["invocation"].to_string())
                    );
                }
                Some("mcp_tool_call_end") => {
                    let _ = writeln!(
                        out,
                        "  [{ts}] mcp_tool_call_end {} result={}",
                        snip(&payload["invocation"].to_string()),
                        snip(&payload["result"].to_string())
                    );
                }
                Some("agent_message") => {
                    let _ = writeln!(
                        out,
                        "  [{ts}] agent_message {}",
                        snip(payload["message"].as_str().unwrap_or("<non-string>"))
                    );
                }
                Some("task_started") => {
                    let _ = writeln!(out, "  [{ts}] task_started turn={}", payload["turn_id"]);
                }
                Some("task_complete") => {
                    let _ = writeln!(
                        out,
                        "  [{ts}] task_complete turn={} last_agent_message={}",
                        payload["turn_id"],
                        snip(payload["last_agent_message"].as_str().unwrap_or("<none>"))
                    );
                }
                Some("turn_aborted") | Some("error") | Some("stream_error") => {
                    let _ = writeln!(
                        out,
                        "  [{ts}] {} {}",
                        payload["type"].as_str().unwrap_or("<event>"),
                        snip(&payload.to_string())
                    );
                }
                _ => {}
            },
            _ => {}
        }
    }
    if out.is_empty() {
        out.push_str("  <no tool calls or notable events parsed>\n");
    }
    out
}

fn snip(text: &str) -> String {
    match text.char_indices().nth(SNIP_CHARS) {
        None => text.to_string(),
        Some((byte_idx, _)) => {
            format!("{}...[+{} bytes]", &text[..byte_idx], text.len() - byte_idx)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_rollout_jsonl_surfaces_tool_calls_args_and_outputs() {
        let transcript = concat!(
            r#"{"timestamp":"T0","type":"session_meta","payload":{"id":"sess-1","cwd":"/w","originator":"neige"}}"#,
            "\n",
            r#"{"timestamp":"T1","type":"response_item","payload":{"type":"function_call","name":"plugin.dev.neige.git-forge_git.commit","arguments":"{}","call_id":"call_1"}}"#,
            "\n",
            r#"{"timestamp":"T2","type":"response_item","payload":{"type":"function_call_output","call_id":"call_1","output":"missing required message"}}"#,
            "\n",
            r#"{"timestamp":"T3","type":"event_msg","payload":{"type":"agent_message","message":"committing now"}}"#,
            "\n",
            r#"{"timestamp":"T4","type":"event_msg","payload":{"type":"turn_aborted","turn_id":"t1","reason":"interrupted"}}"#,
            "\n",
            "not json at all\n",
            r#"{"timestamp":"T5","type":"response_item","payload":{"type":"reasoning","encrypted_content":"xxx"}}"#,
            "\n",
        );
        let summary = summarize_rollout_jsonl(transcript);
        assert!(summary.contains("session_meta id=\"sess-1\""), "{summary}");
        assert!(
            summary.contains(
                "function_call \"plugin.dev.neige.git-forge_git.commit\" call_id=\"call_1\" arguments={}"
            ),
            "{summary}"
        );
        assert!(
            summary.contains("-> output call_id=\"call_1\" \"missing required message\""),
            "{summary}"
        );
        assert!(
            summary.contains("agent_message committing now"),
            "{summary}"
        );
        assert!(summary.contains("turn_aborted"), "{summary}");
        assert!(
            summary.contains("<unparseable json> not json at all"),
            "{summary}"
        );
        assert!(!summary.contains("encrypted_content"), "{summary}");
    }

    #[test]
    fn summarize_rollout_jsonl_handles_empty_transcript() {
        assert_eq!(
            summarize_rollout_jsonl(""),
            "  <no tool calls or notable events parsed>\n"
        );
    }

    #[test]
    fn find_rollout_files_scans_both_codex_home_roots() {
        let root = tempfile::tempdir().expect("tempdir");
        let shared = root.path().join("codex-home/sessions/2026/07/04");
        let per_thread = root.path().join("codex-homes/thread-a/sessions/2026/07/04");
        std::fs::create_dir_all(&shared).expect("mkdir shared sessions");
        std::fs::create_dir_all(&per_thread).expect("mkdir per-thread sessions");
        std::fs::write(shared.join("rollout-2026-07-04T00-00-00-a.jsonl"), "{}").expect("write");
        std::fs::write(per_thread.join("rollout-2026-07-04T00-00-01-b.jsonl"), "{}")
            .expect("write");
        std::fs::write(shared.join("history.jsonl"), "{}").expect("write");
        let found = find_rollout_files(root.path());
        assert_eq!(found.len(), 2, "{found:?}");
    }

    #[test]
    fn evidence_tempdir_cleans_on_success_and_leaks_on_panic() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().to_path_buf();
        drop(EvidenceTempDir::new(dir));
        assert!(!path.exists());

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().to_path_buf();
        let handle = std::thread::spawn(move || {
            let _evidence = EvidenceTempDir::new(dir);
            panic!("intentional test panic for leak-on-panic coverage");
        });
        assert!(handle.join().is_err());
        assert!(path.exists(), "evidence dir must survive panic");
        std::fs::remove_dir_all(&path).expect("cleanup leaked evidence dir");
    }
}
