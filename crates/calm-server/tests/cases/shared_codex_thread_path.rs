//! #2494: `thread_path` takes the rollout file from `thread/read`'s `thread.path` on the real
//! wire. The fixture answers every `thread/read` with `<sock>.thread-read`.
use super::*;

fn script_thread_read(root: &tempfile::TempDir, thread: Value) {
    let sock = root.path().join("run/codex-appserver.sock");
    std::fs::write(
        sock.with_extension("thread-read"),
        json!({ "thread": thread }).to_string(),
    )
    .unwrap();
}

#[tokio::test]
async fn thread_path_answers_the_path_codex_reports_or_none_for_null() {
    let _guard = ENV_LOCK.lock().await;
    let root = tempfile::tempdir().unwrap();
    let daemon = server(&root, repo().await).await;
    daemon.start_or_takeover().await.unwrap();
    let reported = root.path().join("anywhere/thread-a.jsonl");

    script_thread_read(
        &root,
        json!({ "id": "thread-a", "status": { "type": "idle" }, "turns": [],
                "path": reported, "ephemeral": false }),
    );
    assert_eq!(
        daemon.thread_path("thread-a").await.unwrap(),
        Some(reported)
    );

    script_thread_read(
        &root,
        json!({ "id": "thread-a", "status": { "type": "idle" }, "turns": [],
                "path": null, "ephemeral": true }),
    );
    assert_eq!(daemon.thread_path("thread-a").await.unwrap(), None);
}
