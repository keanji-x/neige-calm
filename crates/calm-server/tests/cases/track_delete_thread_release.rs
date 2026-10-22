//! #1853: a committed Track/Area delete also releases the deleted Cards' threads from the shared
//! daemon (`thread/unsubscribe`), so codex can unload them and their MCP servers; a rolled-back
//! delete releases nothing. Driven through the REST routes against the in-process fake daemon.

use super::*;
use calm_truth::db::RepoRead;

/// Every cached thread owned by one of `track_ids`' Cards, sorted: what the delete must release.
async fn cached_threads_of_tracks(b: &Boot, track_ids: &[&str]) -> Vec<String> {
    let mut cards = HashSet::new();
    for track_id in track_ids {
        cards.extend(card_ids(b, track_id).await);
    }
    let mut threads: Vec<String> = resume_candidates(b)
        .into_iter()
        .filter(|(_, card_id)| cards.contains(card_id))
        .map(|(thread_id, _)| thread_id)
        .collect();
    threads.sort();
    threads
}

fn unsubscribed(b: &Boot) -> Vec<String> {
    let mut threads = b.shared_codex.unsubscribed_threads_for_test();
    threads.sort();
    threads
}

#[tokio::test]
async fn deleting_a_track_unsubscribes_its_cards_threads_and_not_its_siblings() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let victim = managed_track(&b, &area_id, "victim").await;
    let survivor = managed_track(&b, &area_id, "survivor").await;
    mint_track_threads(&b, &victim).await;
    mint_track_threads(&b, &survivor).await;
    let victim_threads = cached_threads_of_tracks(&b, &[&victim]).await;
    let survivor_threads = cached_threads_of_tracks(&b, &[&survivor]).await;
    assert!(!victim_threads.is_empty() && !survivor_threads.is_empty());
    assert_eq!(unsubscribed(&b), Vec::<String>::new(), "premise");

    let (status, body) = request(
        b.app.clone(),
        "DELETE",
        &format!("/api/tracks/{victim}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body={body}");

    assert_eq!(
        unsubscribed(&b),
        victim_threads,
        "a committed track delete must unsubscribe exactly the deleted cards' threads"
    );
}

#[tokio::test]
async fn deleting_an_area_unsubscribes_every_member_tracks_threads() {
    let b = boot().await;
    let victim_area = create_area(&b, "Atlas").await;
    let other_area = create_area(&b, "Beta").await;
    let first = managed_track(&b, &victim_area, "first").await;
    let second = managed_track(&b, &victim_area, "second").await;
    let bystander = managed_track(&b, &other_area, "bystander").await;
    for track_id in [&first, &second, &bystander] {
        mint_track_threads(&b, track_id).await;
    }
    let victim_threads = cached_threads_of_tracks(&b, &[&first, &second]).await;

    let (status, body) = request(
        b.app.clone(),
        "DELETE",
        &format!("/api/areas/{victim_area}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body={body}");

    assert_eq!(unsubscribed(&b), victim_threads);
}

#[tokio::test]
async fn a_rolled_back_track_delete_unsubscribes_nothing() {
    let b = boot().await;
    let area_id = create_area(&b, "Atlas").await;
    let track_id = managed_track(&b, &area_id, "survives its own delete").await;
    mint_track_threads(&b, &track_id).await;

    let hook = calm_server::routes::tracks::TrackDeleteCommitHook {
        entered: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
        panic_after_release: true,
    };
    calm_server::routes::tracks::install_track_delete_commit_hook_for_test(&track_id, hook.clone());
    hook.release.notify_one();

    let (status, _) = request(
        b.app.clone(),
        "DELETE",
        &format!("/api/tracks/{track_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(b.repo.track_get(&track_id).await.unwrap().is_some());
    assert_eq!(unsubscribed(&b), Vec::<String>::new());
}
