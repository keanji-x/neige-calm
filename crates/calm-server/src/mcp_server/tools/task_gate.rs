//! `neige_task_gate` (#2464): a gated worker asks the kernel to run its task's gate now, on its
//! current changes committed as the attempt's one commit. The call starts a run or joins the
//! unfinished one, then waits at most the configured bound (D3) and answers `running` or the run's
//! result. The deadline is end to end: it starts with the handler, the admission is one short
//! transaction, and the operation drive runs detached, so a slow drive never holds the call.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use crate::git_candidate::commit_message::CommitMessage;
use crate::mcp_server::framing::{RpcError, calm_error};
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    role_gated_write_annotations,
};
use crate::mcp_server::tools::emit::required_attempt_id;
use crate::model::CardRole;
use crate::operation::OperationRuntime;
use crate::operation::task_gate_run::finalize::{GateRunResult, terminal_result};
use crate::operation::task_gate_run::{
    Admitted, FrozenRun, GATE_RUNS_PER_ATTEMPT, TASK_GATE_RUN_KIND, admit_run_tx,
    gate_run_log_path, running_step,
};

pub const TOOL_TASK_GATE: &str = "neige_task_gate";

/// How often the call re-reads the run's op row while it waits.
const POLL: Duration = Duration::from_millis(100);

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(task_gate_descriptor(), wrap(task_gate));
}

fn wrap<F, Fut>(f: F) -> ToolHandler
where
    F: Fn(Arc<AppContext>, ToolCallIdentity, Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Value, RpcError>> + Send + 'static,
{
    Arc::new(move |ctx, identity, args| -> ToolHandlerFuture {
        let result = f(ctx, identity, args);
        Box::pin(async move {
            result
                .await
                .map(crate::mcp_server::result::ToolResult::structured)
        })
    })
}

fn task_gate_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_TASK_GATE.into(),
        description: include_str!("../../../prompts/tools/neige_task_gate.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["attempt_id"],
            "properties": {
                "attempt_id": { "type": "string", "minLength": 1 },
                "commit_message": { "type": "string" }
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        roles: &[CardRole::Worker],
        listed_for: &[CardRole::Worker],
    }
}

/// `commit_message` under `neige_task_done`'s rule; absent means the kernel's own text.
fn commit_message_arg(args: &Value) -> Result<Option<CommitMessage>, RpcError> {
    match args.get("commit_message") {
        None => Ok(None),
        Some(Value::String(text)) => CommitMessage::parse(text)
            .map(Some)
            .map_err(|error| RpcError::invalid_params(format!("{TOOL_TASK_GATE}: {error}"))),
        Some(_) => Err(RpcError::invalid_params(format!(
            "{TOOL_TASK_GATE}: commit_message must be a string"
        ))),
    }
}

async fn task_gate(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    let deadline = tokio::time::Instant::now() + ctx.gate_run_wait.duration();
    let attempt_id = required_attempt_id(&args, TOOL_TASK_GATE)?;
    let commit_message = commit_message_arg(&args)?;
    let runtime = ctx
        .operation_runtime
        .get()
        .cloned()
        .ok_or_else(|| RpcError::internal("operation runtime not bound"))?;
    let (_, track) =
        crate::mcp_server::tools::track_file::resolve_track_for_identity(&ctx, &identity).await?;
    let card_id = identity.card_id.clone();
    let track_id = track.id.to_string();
    let admit_attempt = attempt_id.clone();
    let admitted = crate::db::write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
        Box::pin(async move {
            admit_run_tx(tx, &admit_attempt, &card_id, &track_id, commit_message).await
        })
    })
    .await
    .map_err(calm_error)?;
    let (key, run, used) = match admitted {
        Admitted::New { key, run, used } | Admitted::Joined { key, run, used } => (key, run, used),
    };
    // Detached: the drive holds one global mutex across every adapter's effects (K18), so the call
    // never awaits it, and dropping the request cancels nothing.
    let drive = runtime.clone();
    tokio::spawn(async move {
        if let Err(error) = drive.drive().await {
            tracing::warn!(%error, "gate run: operation drive failed; recovery re-drives it");
        }
    });
    let log_path = gate_run_log_path(&ctx.gate_logs_dir, &attempt_id, run);
    let header = json!({
        "run": run,
        "log_path": log_path.display().to_string(),
        "runs_used": used,
        "runs_max": GATE_RUNS_PER_ATTEMPT,
    });
    loop {
        if let Some(answer) = read_run(&runtime, &key, run, &log_path).await? {
            return Ok(merge(header, answer));
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            let step = running_frozen(&runtime, &key)
                .await?
                .and_then(|frozen| running_step(&ctx.gate_logs_dir, &frozen));
            return Ok(merge(
                header,
                json!({"commit": null, "state": "running", "step": step}),
            ));
        }
        tokio::time::sleep(POLL.min(deadline - now)).await;
    }
}

/// The finished answer of run `key`, or `None` while it runs.
async fn read_run(
    runtime: &Arc<OperationRuntime>,
    key: &str,
    run: i64,
    log_path: &Path,
) -> Result<Option<Value>, RpcError> {
    let op = runtime
        .find_by_kind_and_idempotency(TASK_GATE_RUN_KIND, key)
        .await
        .map_err(calm_error)?
        .ok_or_else(|| RpcError::internal(format!("gate run {key} has no operation row")))?;
    let Some(result) = runtime.operation_result(&op.id).await.map_err(calm_error)? else {
        return Ok(None);
    };
    Ok(Some(finished(terminal_result(
        run,
        log_path,
        result.outcome,
    ))))
}

async fn running_frozen(
    runtime: &Arc<OperationRuntime>,
    key: &str,
) -> Result<Option<FrozenRun>, RpcError> {
    let op = runtime
        .find_by_kind_and_idempotency(TASK_GATE_RUN_KIND, key)
        .await
        .map_err(calm_error)?;
    Ok(op
        .and_then(|op| op.tx_output)
        .and_then(|output| FrozenRun::from_output(&output).ok()))
}

/// The answer of a finished run: the one renderer the tool and `neige task gate` print.
fn finished(result: GateRunResult) -> Value {
    let verdict = result.verdict;
    let mut answer = json!({
        "commit": result.commit,
        "state": "finished",
        "passed": verdict.passed,
    });
    if !verdict.passed {
        let fields = answer.as_object_mut().expect("answer is an object");
        fields.insert("status_detail".into(), json!(verdict.status_detail));
        fields.insert("failing_step".into(), json!(verdict.failing_step));
        fields.insert("exit_code".into(), json!(verdict.exit_code));
        fields.insert("log_tail".into(), json!(verdict.log_tail));
    }
    answer
}

fn merge(mut header: Value, answer: Value) -> Value {
    if let (Some(header), Value::Object(answer)) = (header.as_object_mut(), answer) {
        header.extend(answer);
    }
    header
}
