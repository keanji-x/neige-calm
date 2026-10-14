//! Socket-scoped release failures and blocked replies exercise the real RPC owner.
use super::*;

pub(super) async fn unsubscribe<S>(
    write: &mut S,
    id: &Value,
    req: &Value,
    reads: &ReadFixtures,
) -> Result<(), String>
where
    S: SinkExt<Message> + Unpin,
    <S as futures::Sink<Message>>::Error: std::fmt::Display,
{
    let requested = req
        .pointer("/params/threadId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    reads.record_line("unsubscribed", requested);
    while reads.sock.with_extension("unsubscribe-park").exists() {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    if reads.sock.with_extension("unsubscribe-fail").exists() {
        return send_error(write, id, -32000, "forced unsubscribe failure").await;
    }
    send_result(write, id, json!({"status":"unsubscribed"})).await
}
