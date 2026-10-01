// Compile as an ordinary calm-server sibling. Inspection cannot issue work.
async fn bypass(client: &crate::codex_appserver::CodexAppServer) {
    let _ = client.thread_start(None).await;
}
