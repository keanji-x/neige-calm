mod support;

use std::sync::Arc;
use std::time::Duration;

use calm_server::db::RepoRead;
use calm_server::db::sqlite::SqlxRepo;

use support::worker_flow as wf;

#[tokio::test]
async fn codex_rollout_source_waits_for_lazy_file_creation() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let thread_id = "thread-lazy";
    let seed = wf::seed_card_and_runtime(&repo, "card-lazy", Some(thread_id)).await;
    let codex_home = tempfile::tempdir().unwrap();
    let path = wf::rollout_path(codex_home.path(), thread_id);

    let (token, handle) = wf::spawn_source_with_discovery(
        repo.clone(),
        seed.runtime.clone(),
        &seed,
        codex_home.path(),
    );
    tokio::time::sleep(Duration::from_millis(80)).await;
    wf::write_rollout(
        &path,
        &[
            wf::session_meta(thread_id),
            wf::user_message("u1", "created later"),
        ],
    );

    wf::wait_until(Duration::from_secs(1), || {
        let repo = repo.clone();
        async move {
            repo.worker_flow_item_list_by_card("card-lazy", 0, 100, false)
                .await
                .unwrap()
                .len()
                == 1
        }
    })
    .await;
    token.cancel();
    handle.await.unwrap().unwrap();
}
