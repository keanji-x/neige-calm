// Positive control: ordinary siblings retain readonly inspection.
async fn inspect(client: &crate::codex_appserver::CodexAppServer) {
    let _ = client.thread_read("thread", true).await;
}
