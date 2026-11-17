use super::*;

pub(super) async fn send_result<S>(write: &mut S, id: &Value, result: Value) -> Result<(), String>
where
    S: SinkExt<Message> + Unpin,
    <S as futures::Sink<Message>>::Error: std::fmt::Display,
{
    let frame = json!({ "jsonrpc": "2.0", "id": id, "result": result });
    write
        .send(Message::Text(frame.to_string()))
        .await
        .map_err(|e| format!("send result: {e}"))
}

pub(super) async fn send_error<S>(
    write: &mut S,
    id: &Value,
    code: i64,
    message: &str,
) -> Result<(), String>
where
    S: SinkExt<Message> + Unpin,
    <S as futures::Sink<Message>>::Error: std::fmt::Display,
{
    let frame = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    });
    write
        .send(Message::Text(frame.to_string()))
        .await
        .map_err(|e| format!("send error: {e}"))
}

pub(super) async fn send_notification<S>(
    write: &mut S,
    method: &str,
    params: Value,
) -> Result<(), String>
where
    S: SinkExt<Message> + Unpin,
    <S as futures::Sink<Message>>::Error: std::fmt::Display,
{
    let frame = json!({ "jsonrpc": "2.0", "method": method, "params": params });
    write
        .send(Message::Text(frame.to_string()))
        .await
        .map_err(|e| format!("send notification: {e}"))
}

/// Per-socket fault injection: consume one method name and close without a response.
pub(super) fn disconnect_once(sock: &std::path::Path, method: &str) -> bool {
    let path = sock.with_extension("disconnect-once");
    std::fs::read_to_string(&path).is_ok_and(|configured| configured.trim() == method)
        && std::fs::remove_file(path).is_ok()
}

/// Socket-owned sequences survive daemon replacement, including a resumed thread's next turn.
/// Allocation has no await on the fixture's single-threaded runtime. First IDs keep the original
/// wire convention, while overlapping connections and later processes cannot reuse them.
pub(super) fn next_id(sock: &std::path::Path, kind: &str) -> Result<String, String> {
    let path = sock.with_extension(format!("{kind}-sequence"));
    let next = match std::fs::read_to_string(&path) {
        Ok(raw) => raw
            .parse::<u64>()
            .map_err(|e| format!("read {}: {e}", path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 1,
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
    std::fs::write(&path, (next + 1).to_string())
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(if kind == "approval" {
        format!("approval-{next:04}")
    } else {
        format!("fake-{kind}-{next:04}")
    })
}
