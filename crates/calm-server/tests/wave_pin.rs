//! Integration tests for `pinned_at` on wave rows.
//!
//! Covers the repo-level round-trip (pin → GET → unpin → GET) and checks that
//! the column survives an unrelated title-only patch without being cleared.

use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::model::*;

async fn fresh_repo() -> SqlxRepo {
    SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open in-memory sqlite repo")
}

async fn make_cove(repo: &SqlxRepo, name: &str) -> Cove {
    repo.cove_create(NewCove {
        name: name.into(),
        color: "#abcdef".into(),
        sort: None,
    })
    .await
    .expect("create cove")
}

async fn make_wave(repo: &SqlxRepo, cove_id: &str, title: &str) -> Wave {
    repo.wave_create(NewWave {
        cove_id: cove_id.into(),
        title: title.into(),
        sort: None,
        cwd: String::new(),
        attach_folder: false,
        theme: calm_server::routes::theme::RequestTheme::default_dark(),
    })
    .await
    .expect("create wave")
}

#[tokio::test]
async fn pinned_at_round_trips_through_patch() {
    let repo = fresh_repo().await;
    let c = make_cove(&repo, "C").await;
    let w = make_wave(&repo, c.id.as_str(), "pin-test").await;

    assert!(w.pinned_at.is_none(), "new wave has no pin");

    let pinned = repo
        .wave_update(
            w.id.as_str(),
            WavePatch {
                pinned_at: Some(Some(12345)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(pinned.pinned_at, Some(12345));

    let re_read = repo.wave_get(w.id.as_str()).await.unwrap().unwrap();
    assert_eq!(re_read.pinned_at, Some(12345));

    let unpinned = repo
        .wave_update(
            w.id.as_str(),
            WavePatch {
                pinned_at: Some(None),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(unpinned.pinned_at, None);

    let re_read2 = repo.wave_get(w.id.as_str()).await.unwrap().unwrap();
    assert_eq!(re_read2.pinned_at, None);
}

#[tokio::test]
async fn omitting_pinned_at_from_patch_leaves_it_alone() {
    let repo = fresh_repo().await;
    let c = make_cove(&repo, "C").await;
    let w = make_wave(&repo, c.id.as_str(), "leave-alone").await;

    let pinned = repo
        .wave_update(
            w.id.as_str(),
            WavePatch {
                pinned_at: Some(Some(99999)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(pinned.pinned_at, Some(99999));

    // Patch title only — pinned_at must be unchanged.
    let title_only = repo
        .wave_update(
            w.id.as_str(),
            WavePatch {
                title: Some("renamed".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(title_only.title, "renamed");
    assert_eq!(
        title_only.pinned_at,
        Some(99999),
        "pinned_at must survive an unrelated title-only patch"
    );
}
