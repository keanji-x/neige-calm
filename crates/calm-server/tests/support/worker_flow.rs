use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use calm_exec::flow::WorkerFlowSource;
use calm_server::db::sqlite::{
    SqlxRepo, card_create_with_id_tx, cove_create_tx, session_start_runtime_tx, wave_create_tx,
};
use calm_server::event::EventBus;
use calm_server::model::{Card, CardRole, NewCard, NewCove, NewWave, RequestTheme};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionProjection,
};
use calm_server::state::{AppState, CodexClient, DaemonClient, WriteContext};
use calm_server::worker_flow::claude_transcript::{
    ClaudeTranscriptFlowSource, ClaudeTranscriptFlowSourceOptions,
};
use calm_server::worker_flow::codex_rollout::{
    CodexRolloutFlowSource, CodexRolloutFlowSourceOptions,
};
use calm_truth::worker_flow_sink::WorkerFlowSink;
use calm_types::worker::{
    LivenessTag, SessionMode, WorkerContract, WorkerProviderKind, WorkerSession, WorkerSessionId,
    WorkerSessionState,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

pub struct SeededRuntime {
    pub card: Card,
    pub runtime: WorkerSessionProjection,
}

pub async fn seed_card_and_runtime(
    repo: &Arc<SqlxRepo>,
    card_id: &str,
    thread_id: Option<&str>,
) -> SeededRuntime {
    seed_card_and_runtime_with_status(repo, card_id, thread_id, WorkerSessionState::Running).await
}

pub async fn seed_card_and_runtime_with_status(
    repo: &Arc<SqlxRepo>,
    card_id: &str,
    thread_id: Option<&str>,
    status: WorkerSessionState,
) -> SeededRuntime {
    let card = seed_codex_card(repo, card_id).await;
    let runtime = seed_runtime_for_card_with_status(repo, &card, thread_id, status).await;
    SeededRuntime { card, runtime }
}

pub async fn seed_claude_card_and_runtime(
    repo: &Arc<SqlxRepo>,
    card_id: &str,
    session_id: &str,
    cwd: &str,
) -> SeededRuntime {
    seed_claude_card_and_runtime_with_status(
        repo,
        card_id,
        session_id,
        cwd,
        WorkerSessionState::Running,
    )
    .await
}

pub async fn seed_claude_card_and_runtime_with_status(
    repo: &Arc<SqlxRepo>,
    card_id: &str,
    session_id: &str,
    cwd: &str,
    status: WorkerSessionState,
) -> SeededRuntime {
    let card = seed_claude_card(repo, card_id, cwd).await;
    let runtime = seed_claude_runtime_for_card_with_status(repo, &card, session_id, status).await;
    SeededRuntime { card, runtime }
}

pub async fn seed_codex_card(repo: &Arc<SqlxRepo>, card_id: &str) -> Card {
    let mut tx = repo.pool().begin().await.unwrap();
    let cove = cove_create_tx(
        &mut tx,
        NewCove {
            name: "cove".into(),
            color: "#fff".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    let wave = wave_create_tx(
        &mut tx,
        NewWave {
            cove_id: cove.id.clone(),
            title: "wave".into(),
            sort: None,
            cwd: "/tmp".into(),
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        repo.wave_cove_cache(),
    )
    .await
    .unwrap();
    let card = card_create_with_id_tx(
        &mut tx,
        card_id.to_string(),
        NewCard {
            wave_id: wave.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: json!({ "task": "test" }),
        },
        CardRole::Worker,
        true,
        repo.card_role_cache(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    card
}

pub async fn seed_claude_card(repo: &Arc<SqlxRepo>, card_id: &str, cwd: &str) -> Card {
    let mut tx = repo.pool().begin().await.unwrap();
    let cove = cove_create_tx(
        &mut tx,
        NewCove {
            name: "cove".into(),
            color: "#fff".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    let wave = wave_create_tx(
        &mut tx,
        NewWave {
            cove_id: cove.id.clone(),
            title: "wave".into(),
            sort: None,
            cwd: cwd.into(),
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        repo.wave_cove_cache(),
    )
    .await
    .unwrap();
    let card = card_create_with_id_tx(
        &mut tx,
        card_id.to_string(),
        NewCard {
            wave_id: wave.id.clone(),
            kind: "claude".into(),
            sort: None,
            payload: json!({ "schemaVersion": 1, "cwd": cwd }),
        },
        CardRole::Worker,
        true,
        repo.card_role_cache(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    card
}

pub async fn seed_runtime_for_card_with_status(
    repo: &Arc<SqlxRepo>,
    card: &Card,
    thread_id: Option<&str>,
    status: WorkerSessionState,
) -> WorkerSessionProjection {
    let mut tx = repo.pool().begin().await.unwrap();
    let runtime = session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: format!("rt-{}", card.id),
            card_id: card.id.as_str().to_string(),
            kind: WorkerSessionKind::CodexCard,
            agent_provider: Some(AgentProvider::Codex),
            status,
            terminal_run_id: None,
            thread_id: thread_id.map(str::to_string),
            session_id: thread_id.map(|id| format!("sess-{id}")),
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: calm_server::model::now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    runtime
}

pub async fn seed_claude_runtime_for_card_with_status(
    repo: &Arc<SqlxRepo>,
    card: &Card,
    session_id: &str,
    status: WorkerSessionState,
) -> WorkerSessionProjection {
    let mut tx = repo.pool().begin().await.unwrap();
    let runtime = session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: format!("rt-{}", card.id),
            card_id: card.id.as_str().to_string(),
            kind: WorkerSessionKind::ClaudeCard,
            agent_provider: Some(AgentProvider::Claude),
            status,
            terminal_run_id: None,
            thread_id: None,
            session_id: Some(session_id.to_string()),
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: calm_server::model::now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    runtime
}

pub fn worker_session(seed: &SeededRuntime) -> WorkerSession {
    WorkerSession {
        id: WorkerSessionId::from(seed.runtime.id.clone()),
        wave_id: seed.card.wave_id.clone(),
        provider: match seed.runtime.agent_provider.as_ref() {
            Some(AgentProvider::Claude) => WorkerProviderKind::Claude,
            Some(AgentProvider::Codex) | None => WorkerProviderKind::Codex,
        },
        mode: SessionMode::Resumable,
        contract: WorkerContract::Executor,
        parent_session_id: None,
        requester_session_id: None,
        state: seed.runtime.status,
        mcp_token_hash: None,
        thread_id: seed.runtime.thread_id.clone(),
        agent_session_id: seed.runtime.session_id.clone(),
        active_turn_id: None,
        terminal_run_id: None,
        card_id: Some(seed.card.id.clone()),
        handle_state_json: None,
        liveness: LivenessTag::Alive,
        liveness_probed_at_ms: None,
        exit_code: None,
        exit_interpretation: None,
        spawn_op_id: None,
        last_activity_ms: None,
        last_thread_status: None,
        created_at_ms: seed.runtime.created_at_ms,
        updated_at_ms: seed.runtime.updated_at_ms,
        completed_at_ms: None,
    }
}

pub fn rollout_path(codex_home: &Path, thread_id: &str) -> PathBuf {
    codex_home
        .join("sessions/2026/06/13")
        .join(format!("rollout-2026-06-13T00-00-00-{thread_id}.jsonl"))
}

pub fn claude_card_cwd(seed: &SeededRuntime) -> String {
    seed.card
        .payload
        .get("cwd")
        .and_then(Value::as_str)
        .unwrap_or("/tmp")
        .to_string()
}

pub fn claude_system(uuid: &str, cwd: &str) -> Value {
    json!({
        "type": "system",
        "subtype": "init",
        "uuid": uuid,
        "timestamp": "2026-06-13T00:00:00Z",
        "cwd": cwd
    })
}

pub fn claude_user_string(uuid: &str, text: &str) -> Value {
    json!({
        "type": "user",
        "uuid": uuid,
        "timestamp": "2026-06-13T00:00:01Z",
        "message": {
            "role": "user",
            "content": text
        }
    })
}

pub fn claude_user_blocks(uuid: &str, blocks: Vec<Value>) -> Value {
    json!({
        "type": "user",
        "uuid": uuid,
        "timestamp": "2026-06-13T00:00:02Z",
        "message": {
            "role": "user",
            "content": blocks
        }
    })
}

pub fn claude_tool_result(tool_use_id: &str, content: &str, is_error: bool) -> Value {
    json!({
        "type": "tool_result",
        "tool_use_id": tool_use_id,
        "content": content,
        "is_error": is_error
    })
}

pub fn claude_assistant(uuid: &str, cwd: &str, blocks: Vec<Value>) -> Value {
    json!({
        "type": "assistant",
        "uuid": uuid,
        "timestamp": "2026-06-13T00:00:03Z",
        "message": {
            "role": "assistant",
            "cwd": cwd,
            "content": blocks
        }
    })
}

pub fn claude_thinking(text: &str) -> Value {
    json!({ "type": "thinking", "thinking": text })
}

pub fn claude_text(text: &str) -> Value {
    json!({ "type": "text", "text": text })
}

pub fn claude_tool_use(id: &str, name: &str, input: Value) -> Value {
    json!({
        "type": "tool_use",
        "id": id,
        "name": name,
        "input": input
    })
}

pub fn claude_attachment(uuid: &str, cwd: &str) -> Value {
    json!({
        "type": "attachment",
        "hook_event": "PostToolUse",
        "uuid": uuid,
        "timestamp": "2026-06-13T00:00:04Z",
        "cwd": cwd,
        "command": "echo hook",
        "stdout": "ok",
        "stderr": "",
        "exitCode": 0
    })
}

pub fn session_meta(thread_id: &str) -> Value {
    json!({
        "timestamp": "2026-06-13T00:00:00Z",
        "type": "session_meta",
        "payload": {
            "id": thread_id,
            "timestamp": "2026-06-13T00:00:00Z",
            "cwd": "/tmp",
            "originator": "test",
            "cli_version": "test"
        }
    })
}

pub fn user_message(id: &str, text: &str) -> Value {
    json!({
        "timestamp": "2026-06-13T00:00:01Z",
        "type": "response_item",
        "payload": {
            "type": "message",
            "id": id,
            "role": "user",
            "content": [{ "type": "input_text", "text": text }]
        }
    })
}

pub fn assistant_message(id: &str, text: &str) -> Value {
    json!({
        "timestamp": "2026-06-13T00:00:02Z",
        "type": "response_item",
        "payload": {
            "type": "message",
            "id": id,
            "role": "assistant",
            "phase": "final_answer",
            "content": [{ "type": "output_text", "text": text }]
        }
    })
}

pub fn reasoning(id: &str, text: &str) -> Value {
    json!({
        "timestamp": "2026-06-13T00:00:03Z",
        "type": "response_item",
        "payload": {
            "type": "reasoning",
            "id": id,
            "summary": [{ "type": "summary_text", "text": text }],
            "content": [{ "type": "reasoning_text", "text": text }]
        }
    })
}

pub fn function_call(call_id: &str, command: &str) -> Value {
    json!({
        "timestamp": "2026-06-13T00:00:04Z",
        "type": "response_item",
        "payload": {
            "type": "function_call",
            "name": "exec_command",
            "arguments": serde_json::to_string(&json!({ "cmd": ["bash", "-lc", command], "cwd": "/tmp" })).unwrap(),
            "call_id": call_id
        }
    })
}

pub fn function_output(call_id: &str, text: &str) -> Value {
    json!({
        "timestamp": "2026-06-13T00:00:05Z",
        "type": "response_item",
        "payload": {
            "type": "function_call_output",
            "call_id": call_id,
            "output": text
        }
    })
}

pub fn exec_command_begin(call_id: &str) -> Value {
    json!({
        "timestamp": "2026-06-13T00:00:05Z",
        "type": "event_msg",
        "payload": {
            "type": "exec_command_begin",
            "call_id": call_id,
            "command": ["bash", "-lc", "echo hi"],
            "cwd": "/tmp",
            "source": "agent"
        }
    })
}

pub fn exec_command_end(
    call_id: &str,
    status: &str,
    exit_code: i32,
    aggregated_output: &str,
    duration_ms: i64,
    command: &[&str],
) -> Value {
    json!({
        "timestamp": "2026-06-13T00:00:05Z",
        "type": "event_msg",
        "payload": {
            "type": "exec_command_end",
            "call_id": call_id,
            "command": command,
            "cwd": "/tmp",
            "parsed_cmd": [],
            "aggregated_output": aggregated_output,
            "exit_code": exit_code,
            "duration": {
                "secs": duration_ms / 1_000,
                "nanos": (duration_ms % 1_000) * 1_000_000
            },
            "status": status,
            "source": "agent"
        }
    })
}

pub fn custom_patch(call_id: &str) -> Value {
    json!({
        "timestamp": "2026-06-13T00:00:06Z",
        "type": "response_item",
        "payload": {
            "type": "custom_tool_call",
            "call_id": call_id,
            "name": "apply_patch",
            "input": "*** Begin Patch\n*** Update File: src/main.rs\n@@\n-old\n+new\n*** End Patch",
            "status": "completed"
        }
    })
}

pub fn web_search(id: &str, query: &str) -> Value {
    json!({
        "timestamp": "2026-06-13T00:00:07Z",
        "type": "response_item",
        "payload": {
            "type": "web_search_call",
            "id": id,
            "status": "completed",
            "action": { "type": "search", "query": query }
        }
    })
}

pub fn turn_context(turn_id: &str) -> Value {
    json!({
        "timestamp": "2026-06-13T00:00:08Z",
        "type": "turn_context",
        "payload": { "turn_id": turn_id, "cwd": "/tmp", "model": "test" }
    })
}

pub fn compacted(summary: &str) -> Value {
    json!({
        "timestamp": "2026-06-13T00:00:09Z",
        "type": "compacted",
        "payload": { "message": summary }
    })
}

pub fn write_rollout(path: &Path, lines: &[Value]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let mut body = String::new();
    for line in lines {
        body.push_str(&serde_json::to_string(line).unwrap());
        body.push('\n');
    }
    std::fs::write(path, body).unwrap();
}

pub fn write_transcript(path: &Path, lines: &[Value]) {
    write_rollout(path, lines);
}

pub fn append_rollout(path: &Path, lines: &[Value]) {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    for line in lines {
        writeln!(file, "{}", serde_json::to_string(line).unwrap()).unwrap();
    }
}

pub fn append_transcript(path: &Path, lines: &[Value]) {
    append_rollout(path, lines);
}

pub async fn wait_until<F, Fut>(timeout: Duration, mut condition: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let start = Instant::now();
    while start.elapsed() < timeout {
        if condition().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("condition not met within {timeout:?}");
}

pub fn app_state(repo: Arc<SqlxRepo>, events: EventBus) -> AppState {
    AppState::from_parts(
        repo.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo,
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-worker-flow"),
            Vec::new(),
            events,
            WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::wave_cove_cache::WaveCoveCache::new(),
            ),
        )),
        Arc::new(CodexClient::new_stub()),
        None,
        None,
    )
}

pub fn spawn_source_with_path(
    repo: Arc<SqlxRepo>,
    runtime: WorkerSessionProjection,
    seed: &SeededRuntime,
    path: &Path,
) -> (
    CancellationToken,
    tokio::task::JoinHandle<Result<(), calm_types::error::CoreError>>,
) {
    let token = CancellationToken::new();
    let source = CodexRolloutFlowSource::new_with_options(
        repo.clone(),
        runtime,
        path.parent()
            .unwrap_or_else(|| Path::new("/"))
            .to_path_buf(),
        token.clone(),
        CodexRolloutFlowSourceOptions {
            path_override: Some(path.to_path_buf()),
            poll_interval: Duration::from_millis(20),
            lazy_retry_delay: Duration::from_millis(10),
            lazy_retry_attempts: 3,
            ..CodexRolloutFlowSourceOptions::default()
        },
    );
    let session = worker_session(seed);
    let sink = WorkerFlowSink::new(repo);
    let handle = tokio::spawn(async move { source.capture(&session, &sink).await });
    (token, handle)
}

pub fn spawn_source_with_discovery(
    repo: Arc<SqlxRepo>,
    runtime: WorkerSessionProjection,
    seed: &SeededRuntime,
    codex_home: &Path,
) -> (
    CancellationToken,
    tokio::task::JoinHandle<Result<(), calm_types::error::CoreError>>,
) {
    let token = CancellationToken::new();
    let source = CodexRolloutFlowSource::new_with_options(
        repo.clone(),
        runtime,
        codex_home.to_path_buf(),
        token.clone(),
        CodexRolloutFlowSourceOptions {
            path_override: None,
            poll_interval: Duration::from_millis(20),
            lazy_retry_delay: Duration::from_millis(10),
            lazy_retry_attempts: 3,
            ..CodexRolloutFlowSourceOptions::default()
        },
    );
    let session = worker_session(seed);
    let sink = WorkerFlowSink::new(repo);
    let handle = tokio::spawn(async move { source.capture(&session, &sink).await });
    (token, handle)
}

pub fn spawn_claude_source_with_path(
    repo: Arc<SqlxRepo>,
    runtime: WorkerSessionProjection,
    seed: &SeededRuntime,
    path: &Path,
) -> (
    CancellationToken,
    tokio::task::JoinHandle<Result<(), calm_types::error::CoreError>>,
) {
    let token = CancellationToken::new();
    let source = ClaudeTranscriptFlowSource::new_with_options(
        repo.clone(),
        runtime,
        claude_card_cwd(seed),
        token.clone(),
        ClaudeTranscriptFlowSourceOptions {
            path_override: Some(path.to_path_buf()),
            poll_interval: Duration::from_millis(20),
            lazy_retry_delay: Duration::from_millis(10),
            lazy_retry_attempts: 3,
            ..ClaudeTranscriptFlowSourceOptions::default()
        },
    );
    let session = worker_session(seed);
    let sink = WorkerFlowSink::new(repo);
    let handle = tokio::spawn(async move { source.capture(&session, &sink).await });
    (token, handle)
}
