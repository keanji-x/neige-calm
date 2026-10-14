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
