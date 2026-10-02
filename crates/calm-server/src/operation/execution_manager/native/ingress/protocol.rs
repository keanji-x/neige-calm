use super::super::super::native::supervisor::execution_backend::{CodexBackend, TurnRequest};
use super::super::super::{Owner, WritePermit, storage};
use super::*;

#[derive(Clone, Copy)]
enum Method {
    Inspection,
    ThreadInspection,
    ThreadList,
    ThreadStart,
    ThreadResume,
    TurnStart,
    Steer,
    Interrupt,
    Unsubscribe,
}
fn classify(method: &str) -> Result<Method> {
    Ok(match method {
        "model/list" | "config/read" | "account/read" => Method::Inspection,
        "thread/read" | "thread/backgroundTerminals/list" => Method::ThreadInspection,
        "thread/list" | "thread/loaded/list" => Method::ThreadList,
        "thread/start" => Method::ThreadStart,
        "thread/resume" => Method::ThreadResume,
        "turn/start" => Method::TurnStart,
        "turn/steer" => Method::Steer,
        "turn/interrupt" => Method::Interrupt,
        "thread/unsubscribe" => Method::Unsubscribe,
        _ => {
            return Err(CalmError::Conflict(
                "unsupported native ingress method; mutation denied".into(),
            ));
        }
    })
}
fn thread(params: &Value) -> Result<&str> {
    params
        .get("threadId")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| CalmError::Conflict("native request requires threadId".into()))
}
/// 0.159.2 nullable fields that would add another execution/capability contract.
/// Accept SDK omissions explicitly; enabling those features needs its own manager plan.
fn disabled_fields(params: &mut Value, keys: &[&str]) -> Result<()> {
    let object = params
        .as_object_mut()
        .ok_or_else(|| CalmError::Conflict("invalid native params".into()))?;
    for key in keys {
        if let Some(value) = object.remove(*key) {
            if !value.is_null() {
                return Err(CalmError::Conflict(format!(
                    "unsupported managed native field {key}"
                )));
            }
        }
    }
    Ok(())
}
fn frozen_params(scope: &Scope, params: &mut Value, create: bool) -> Result<()> {
    let object = params
        .as_object_mut()
        .ok_or_else(|| CalmError::Conflict("native params must be an object".into()))?;
    if let Some(cwd) = object.get("cwd").filter(|v| !v.is_null()) {
        let cwd = cwd
            .as_str()
            .ok_or_else(|| CalmError::Conflict("invalid native cwd".into()))?;
        if std::fs::canonicalize(cwd)? != Path::new(&scope.cwd) {
            return Err(CalmError::Conflict(
                "session workspace is frozen; open the corresponding card".into(),
            ));
        }
    }
    for (key, allowed) in [("approvalPolicy", "never"), ("sandbox", "workspace-write")] {
        if let Some(value) = object.get(key).filter(|v| !v.is_null()) {
            if value.as_str() != Some(allowed) {
                return Err(CalmError::Conflict(
                    "session permission policy is frozen".into(),
                ));
            }
        }
        object.remove(key);
    }
    for key in [
        "permissions",
        "sandboxPolicy",
        "config",
        "baseInstructions",
        "developerInstructions",
    ] {
        if let Some(value) = object.get(key).filter(|v| !v.is_null()) {
            if value != &json!({}) {
                return Err(CalmError::Conflict(
                    "session permission/configuration overrides are unsupported".into(),
                ));
            }
        }
        object.remove(key);
    }
    if object
        .get("ephemeral")
        .is_some_and(|value| !value.is_null() && value != &json!(false))
    {
        return Err(CalmError::Conflict(
            "managed sessions require durable provider history".into(),
        ));
    }
    object.insert("runtimeWorkspaceRoots".into(), json!([scope.cwd]));
    if create {
        object.insert("cwd".into(), json!(scope.cwd));
        object.insert("approvalPolicy".into(), json!("never"));
        // The scope was selected by the manager, never by a client.
        match &scope.permissions {
            PermissionsChoice::SandboxMode(mode) => {
                object.insert("sandbox".into(), json!(mode));
            }
            PermissionsChoice::NamedProfile(profile) => {
                object.insert("permissions".into(), json!(profile));
            }
        }
    } else {
        object.remove("cwd");
    }
    Ok(())
}
fn validate_fields(params: &Value, allowed: &[&str]) -> Result<()> {
    let object = params
        .as_object()
        .ok_or_else(|| CalmError::Conflict("invalid native params".into()))?;
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(CalmError::Conflict(
            "unsupported native mutation parameter".into(),
        ));
    }
    Ok(())
}

pub(super) async fn request(
    pool: &SqlitePool,
    scope: &Scope,
    provider: &Arc<CodexAppServer>,
    shared: &super::super::SharedCodexAppServer,
    frame: Value,
) -> Result<Value> {
    scope::require_open(pool, scope).await?;
    let method = frame["method"]
        .as_str()
        .ok_or_else(|| CalmError::Conflict("native method required".into()))?;
    let kind = classify(method)?;
    let mut params = frame.get("params").cloned().unwrap_or_else(|| json!({}));
    match kind {
        Method::Inspection => {
            if let Some(cwd) = params.get("cwd").filter(|value| !value.is_null()) {
                if std::fs::canonicalize(
                    cwd.as_str()
                        .ok_or_else(|| CalmError::Conflict("invalid inspection cwd".into()))?,
                )? != Path::new(&scope.cwd)
                {
                    return Err(CalmError::Conflict(
                        "inspection scope differs from session".into(),
                    ));
                }
            }
            provider.request_envelope(method, params).await
        }
        Method::ThreadInspection | Method::Unsubscribe => {
            let thread = thread(&params)?;
            scope::require_thread(pool, scope, thread).await?;
            provider.request_envelope(method, params).await
        }
        Method::ThreadList => {
            let owned: Vec<String> = sqlx::query_scalar(
                "SELECT holder_id FROM workspace_execution_bindings \
                 WHERE provider='codex' AND card_id=?1 AND cwd=?2 AND scope_phase IN ('new','ready')"
            ).bind(&scope.card).bind(&scope.cwd).fetch_all(pool).await?;
            let mut reply = provider.request_envelope(method, params).await?;
            if let Some(data) = reply
                .pointer_mut("/result/data")
                .and_then(Value::as_array_mut)
            {
                data.retain(|value| {
                    (if method == "thread/loaded/list" {
                        value.as_str()
                    } else {
                        value.get("id").and_then(Value::as_str)
                    })
                    .is_some_and(|id| owned.iter().any(|t| t == id))
                });
            }
            if let Some(data) = reply
                .pointer_mut("/result/threadIds")
                .and_then(Value::as_array_mut)
            {
                data.retain(|value| {
                    value
                        .as_str()
                        .is_some_and(|id| owned.iter().any(|t| t == id))
                });
            }
            Ok(reply)
        }
        Method::ThreadStart => {
            disabled_fields(
                &mut params,
                &[
                    "approvalsReviewer",
                    "daybreakEnabled",
                    "dynamicTools",
                    "environments",
                    "mockExperimentalField",
                    "multiAgentMode",
                    "projectId",
                    "runtimeWorkspaceRoots",
                    "selectedCapabilityRoots",
                    "serviceName",
                ],
            )?;
            validate_fields(
                &params,
                &[
                    "cwd",
                    "model",
                    "modelProvider",
                    "approvalPolicy",
                    "sandbox",
                    "permissions",
                    "config",
                    "baseInstructions",
                    "developerInstructions",
                    "personality",
                    "ephemeral",
                    "serviceTier",
                    "experimentalRawEvents",
                    "allowProviderModelFallback",
                    "historyMode",
                    "sessionStartSource",
                    "threadSource",
                ],
            )?;
            if params
                .get("allowProviderModelFallback")
                .is_some_and(|value| value != &json!(false))
            {
                return Err(CalmError::Conflict(
                    "managed session model fallback is unsupported".into(),
                ));
            }
            frozen_params(scope, &mut params, true)?;
            let _start = shared.ingress_thread_start_guard().await;
            let mut tx = crate::db::sqlite::begin_immediate_tx(pool).await?;
            let inserted = sqlx::query(
                "INSERT INTO native_session_thread_requests(session_execution_id,request_json) \
                 SELECT ingress.session_execution_id,?2 FROM native_session_ingresses ingress \
                 JOIN workspace_leases lease ON lease.lease_id=ingress.session_execution_id \
                 WHERE ingress.session_execution_id=?1 AND lease.state='held' \
                 AND lease.holder_phase='running' \
                 AND NOT EXISTS(SELECT 1 FROM native_session_thread_requests previous \
                 WHERE previous.session_execution_id=?1 AND previous.reply_json IS NULL)",
            )
            .bind(&scope.execution)
            .bind(params.to_string())
            .execute(&mut *tx)
            .await?;
            if inserted.rows_affected() != 1 {
                return Err(CalmError::Conflict(
                    "session thread creation remains unconfirmed; recover its existing thread"
                        .into(),
                ));
            }
            let request_id = inserted.last_insert_rowid();
            let record = storage::load_in(&mut tx, &scope.execution)
                .await?
                .ok_or_else(|| CalmError::Conflict("session reservation disappeared".into()))?;
            tx.commit().await?;
            let permit = WritePermit {
                nonce: record.id.clone(),
                record,
                policy: Some(scope.permissions.clone()),
            };
            let reply = thread_mutation(provider, permit, method, params).await?;
            if reply.get("error").is_some() && reply.get("result").is_some() {
                return Err(CalmError::Conflict(
                    "thread reply contradicts its outcome; creation unconfirmed".into(),
                ));
            }
            if let Some(error) = reply.get("error") {
                if error["code"].as_i64().is_none() || error["message"].as_str().is_none() {
                    return Err(CalmError::Conflict(
                        "thread refusal remains unconfirmed".into(),
                    ));
                }
                record_thread_reply(pool, request_id, &reply).await?;
                return Ok(reply);
            }
            let thread = reply
                .pointer("/result/thread/id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .ok_or_else(|| CalmError::Conflict("thread creation remains unconfirmed".into()))?;
            let cwd = reply
                .pointer("/result/thread/cwd")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    CalmError::Conflict("created thread workspace is unconfirmed".into())
                })?;
            if std::fs::canonicalize(cwd)? != Path::new(&scope.cwd) {
                return Err(CalmError::Conflict(
                    "created thread differs from frozen scope".into(),
                ));
            }
            shared
                .bind_ingress_thread(&scope.terminal, &scope.card, thread, &scope.cwd)
                .await?;
            scope::associate_thread(pool, scope, thread).await?;
            record_thread_reply(pool, request_id, &reply).await?;
            Ok(reply)
        }
        Method::ThreadResume => {
            disabled_fields(
                &mut params,
                &[
                    "approvalsReviewer",
                    "history",
                    "path",
                    "runtimeWorkspaceRoots",
                ],
            )?;
            validate_fields(
                &params,
                &[
                    "threadId",
                    "cwd",
                    "model",
                    "modelProvider",
                    "approvalPolicy",
                    "sandbox",
                    "permissions",
                    "config",
                    "baseInstructions",
                    "developerInstructions",
                    "personality",
                    "excludeTurns",
                    "initialTurnsPage",
                    "serviceTier",
                ],
            )?;
            let thread = thread(&params)?.to_owned();
            scope::require_thread(pool, scope, &thread).await?;
            let facts = provider.thread_workspace_history(&thread).await?;
            if facts.thread.id != thread
                || std::fs::canonicalize(facts.thread.cwd)? != Path::new(&scope.cwd)
            {
                return Err(CalmError::Conflict(
                    "provider resume scope differs; open the corresponding card".into(),
                ));
            }
            frozen_params(scope, &mut params, true)?;
            scope::associate_thread(pool, scope, &thread).await?;
            let record = storage::load(pool, &scope.execution)
                .await?
                .ok_or_else(|| CalmError::Conflict("session reservation disappeared".into()))?;
            let permit = WritePermit {
                nonce: record.id.clone(),
                record,
                policy: Some(scope.permissions.clone()),
            };
            thread_mutation(provider, permit, method, params).await
        }
        Method::TurnStart => {
            disabled_fields(
                &mut params,
                &[
                    "approvalsReviewer",
                    "cyberAccessProgram",
                    "disabledPluginIds",
                    "environments",
                    "multiAgentMode",
                    "runtimeWorkspaceRoots",
                    "toolOutput",
                ],
            )?;
            validate_fields(
                &params,
                &[
                    "threadId",
                    "input",
                    "cwd",
                    "model",
                    "effort",
                    "summary",
                    "personality",
                    "serviceTier",
                    "clientUserMessageId",
                    "approvalPolicy",
                    "sandboxPolicy",
                    "permissions",
                    "outputSchema",
                    "collaborationMode",
                    "additionalContext",
                    "responsesapiClientMetadata",
                    "serviceTierForTurn",
                    "turnTrigger",
                ],
            )?;
            let thread = thread(&params)?.to_owned();
            scope::require_thread(pool, scope, &thread).await?;
            frozen_params(scope, &mut params, false)?;
            let backend = CodexBackend::for_ingress(provider.clone());
            let manager = ExecutionManager::new(pool.clone());
            let submission = manager
                .submit_for_session(
                    &backend,
                    &Owner {
                        card: scope.card.clone(),
                        holder: thread.clone(),
                    },
                    TurnRequest { thread, params },
                    None,
                    None,
                    Some(&scope.execution),
                )
                .await?;
            Ok(match submission {
                super::super::super::Submission::Started(receipt) => receipt.output,
                super::super::super::Submission::Rejected { output, .. } => output,
            })
        }
        Method::Steer => {
            validate_fields(
                &params,
                &[
                    "threadId",
                    "expectedTurnId",
                    "input",
                    "clientUserMessageId",
                    "additionalContext",
                    "responsesapiClientMetadata",
                ],
            )?;
            let thread = thread(&params)?.to_owned();
            scope::require_thread(pool, scope, &thread).await?;
            let turn = params["expectedTurnId"]
                .as_str()
                .ok_or_else(|| CalmError::Conflict("steer requires turn generation".into()))?;
            let manager = ExecutionManager::new(pool.clone());
            let backend = CodexBackend::for_ingress(provider.clone());
            manager
                .steer_native_protocol(&backend, &scope.execution, &thread, turn, params.clone())
                .await
        }
        Method::Interrupt => {
            validate_fields(&params, &["threadId", "turnId"])?;
            let thread = thread(&params)?.to_owned();
            scope::require_thread(pool, scope, &thread).await?;
            let turn = params["turnId"]
                .as_str()
                .ok_or_else(|| CalmError::Conflict("interrupt requires turn generation".into()))?;
            let manager = ExecutionManager::new(pool.clone());
            let backend = CodexBackend::for_ingress(provider.clone());
            manager.interrupt_native(&backend, &thread, turn).await?;
            Ok(json!({"jsonrpc":"2.0","result":{}}))
        }
    }
}
async fn record_thread_reply(pool: &SqlitePool, request: i64, reply: &Value) -> Result<()> {
    sqlx::query("UPDATE native_session_thread_requests SET reply_json=?2 WHERE id=?1")
        .bind(request)
        .bind(reply.to_string())
        .execute(pool)
        .await?;
    Ok(())
}
async fn thread_mutation(
    provider: &CodexAppServer,
    permit: WritePermit,
    method: &str,
    params: Value,
) -> Result<Value> {
    if permit.record.backend != super::super::super::BackendKind::NativeSession
        || permit.record.phase != "running"
    {
        return Err(CalmError::Conflict(
            "native thread request has no live session authority".into(),
        ));
    }
    provider.request_envelope(method, params).await
}

type Callbacks = std::collections::HashMap<String, (String, String)>;
pub(super) async fn provider_event(
    pool: &SqlitePool,
    scope: &Scope,
    provider: &CodexAppServer,
    callbacks: &mut Callbacks,
    frame: &Value,
) -> Result<bool> {
    if frame.get("method").is_none() {
        sqlx::query("INSERT INTO native_session_unmatched_replies(session_execution_id,frame_json,received_at_ms) VALUES(?1,?2,?3)")
            .bind(&scope.execution).bind(frame.to_string()).bind(crate::model::now_ms()).execute(pool).await?;
        return Ok(false);
    }
    let params = &frame["params"];
    let thread = params
        .get("threadId")
        .or_else(|| params.pointer("/thread/id"))
        .and_then(Value::as_str);
    let owned = if let Some(thread) = thread {
        scope::require_thread(pool, scope, thread).await.is_ok()
    } else {
        false
    };
    if frame.get("id").is_some() {
        let method = frame["method"].as_str().unwrap_or("");
        let turn = params.get("turnId").and_then(Value::as_str);
        // Approval policy is manager-selected `never`; only known user questions bridge.
        let supported = method == "item/tool/requestUserInput" && owned && callbacks.len() < 32;
        if supported {
            if let (Some(thread), Some(turn)) = (thread, turn) {
                if storage::running_native_turn(pool, thread, turn)
                    .await?
                    .is_some()
                {
                    callbacks.insert(frame["id"].to_string(), (thread.into(), turn.into()));
                    return Ok(true);
                }
            }
        }
        provider.send_protocol_frame(json!({"jsonrpc":"2.0","id":frame["id"],"error":{"code":-32601,"message":"unsupported or unauthorized managed server request"}})).await?;
        return Ok(false);
    }
    Ok(owned)
}
pub(super) async fn client_callback(
    pool: &SqlitePool,
    scope: &Scope,
    provider: &Arc<CodexAppServer>,
    callbacks: &mut Callbacks,
    frame: Value,
) -> Result<()> {
    scope::require_open(pool, scope).await?;
    let (thread, turn) = callbacks
        .remove(&frame["id"].to_string())
        .ok_or_else(|| CalmError::Conflict("uncorrelated native server response".into()))?;
    scope::require_thread(pool, scope, &thread).await?;
    let backend = CodexBackend::for_ingress(provider.clone());
    ExecutionManager::new(pool.clone())
        .reply_native_protocol(&backend, &scope.execution, &thread, &turn, frame)
        .await
}
