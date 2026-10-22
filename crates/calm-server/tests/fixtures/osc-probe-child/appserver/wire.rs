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
