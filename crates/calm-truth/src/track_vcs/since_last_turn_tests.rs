use super::store::{CommitTreeMeta, commit_tree_at_tx, put_rendered_entry, store_tree};
use super::types::BlobContent;
use super::*;
use crate::db::prelude::*;
use crate::db::sqlite::SqlxRepo;
use crate::ids::TrackId;
use crate::model::{NewArea, NewTrack, RequestTheme};
use calm_types::organization::LEGACY_OWNER_USER_ID;
use std::collections::BTreeMap;

async fn fixture() -> (SqlxRepo, TrackId) {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = repo
        .area_create(
            LEGACY_OWNER_USER_ID,
            NewArea {
                name: "area".into(),
                color: "#336699".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id,
            title: "track".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    (repo, track.id)
}

async fn commit(
    repo: &SqlxRepo,
    track: &TrackId,
    parent: Option<&str>,
    author: Option<&str>,
    files: &[(&str, &str)],
) -> String {
    let mut tx = repo.pool().begin().await.unwrap();
    let mut entries = BTreeMap::new();
    for (path, text) in files {
        put_rendered_entry(
            &mut tx,
            &mut entries,
            *path,
            BlobContent {
                bytes: text.as_bytes().to_vec(),
                content_type: "text/plain".into(),
            },
            1,
        )
        .await
        .unwrap();
    }
    static CLOCK: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(1);
    let created_at = CLOCK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tree = store_tree(&mut tx, MANIFEST_SCHEMA_VERSION, entries, 1)
        .await
        .unwrap();
    let hash = commit_tree_at_tx(
        &mut tx,
        track,
        &tree,
        CommitTreeMeta {
            parent_hash: parent,
            author,
            event_id: None,
            message: "test",
            manifest_schema_version: MANIFEST_SCHEMA_VERSION,
            created_at,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    hash
}

#[tokio::test]
async fn report_knowledge_delta_vcs_header_omits_report_for_every_author() {
    let (repo, track) = fixture().await;
    let before = commit(&repo, &track, None, None, &[("report.md", "old\n")]).await;
    for author in [
        Some("ai:planner-session:this"),
        Some("user"),
        Some("plugin:test"),
        Some("ai:assistant-session:a"),
        Some("kernel"),
        Some("ai:planner-session:other"),
        None,
    ] {
        let after = commit(
            &repo,
            &track,
            Some(&before),
            author,
            &[("report.md", "changed\n")],
        )
        .await;
        let block = since_last_turn_block(
            repo.pool(),
            &track,
            Some(&before),
            Some(&after),
            None,
            &calm_types::worker::WorkerSessionId::from("this"),
            ReportPatch::Include,
        )
        .await
        .unwrap()
        .block;
        assert!(
            block.is_none(),
            "Report is presented by its knowledge owner: {block:?}"
        );
        let mixed = commit(
            &repo,
            &track,
            Some(&after),
            Some("ai:planner-session:this"),
            &[("report.md", "own final\n")],
        )
        .await;
        let block = since_last_turn_block(
            repo.pool(),
            &track,
            Some(&before),
            Some(&mixed),
            None,
            &calm_types::worker::WorkerSessionId::from("this"),
            ReportPatch::Include,
        )
        .await
        .unwrap()
        .block;
        assert!(
            block.is_none(),
            "Report has no author fallback in the VCS header: {block:?}"
        );
    }
}

#[tokio::test]
async fn since_last_turn_collapses_card_internals_but_keeps_runs() {
    let (repo, track) = fixture().await;
    let before = commit(&repo, &track, None, None, &[("cards/a/.meta.json", "old")]).await;
    let after = commit(
        &repo,
        &track,
        Some(&before),
        Some("user"),
        &[
            ("cards/a/.meta.json", "new"),
            ("cards/a/.payload.json", "new"),
            ("cards/a/conversation.md", "new"),
            ("cards/a/events.json", "new"),
            ("cards/b/.meta.json", "new"),
            ("cards/index.json", "new"),
            ("index.md", "new"),
            ("runs/attempt.json", "new"),
        ],
    )
    .await;
    let block = since_last_turn_block(
        repo.pool(),
        &track,
        Some(&before),
        Some(&after),
        None,
        &calm_types::worker::WorkerSessionId::from("this"),
        ReportPatch::Include,
    )
    .await
    .unwrap()
    .block
    .unwrap();
    assert!(block.contains("卡片 a 更新"), "{block}");
    assert!(block.contains("卡片 b 新建"), "{block}");
    assert!(block.contains("runs/attempt.json new"), "{block}");
    assert!(!block.contains("cards/"), "{block}");
    assert!(!block.contains("index.md"), "{block}");
}

#[tokio::test]
async fn report_knowledge_delta_vcs_header_has_no_mixed_or_unknown_fallback() {
    let (repo, track) = fixture().await;
    let before = commit(&repo, &track, None, None, &[("report.md", "old\n")]).await;
    let other = commit(
        &repo,
        &track,
        Some(&before),
        Some("user"),
        &[("report.md", "other\n")],
    )
    .await;
    let own = commit(
        &repo,
        &track,
        Some(&other),
        Some("ai:planner-session:this"),
        &[("report.md", "old\n")],
    )
    .await;
    let session = calm_types::worker::WorkerSessionId::from("this");
    let block = since_last_turn_block(
        repo.pool(),
        &track,
        Some(&before),
        Some(&own),
        None,
        &session,
        ReportPatch::Include,
    )
    .await
    .unwrap()
    .block;
    assert!(
        block.is_none(),
        "Report has no mixed-author fallback: {block:?}"
    );
    let mut current = other;
    for _ in 0..50 {
        current = commit(
            &repo,
            &track,
            Some(&current),
            Some("ai:planner-session:this"),
            &[("report.md", "other\n")],
        )
        .await;
    }
    let block = since_last_turn_block(
        repo.pool(),
        &track,
        Some(&before),
        Some(&current),
        None,
        &session,
        ReportPatch::Include,
    )
    .await
    .unwrap()
    .block;
    assert!(
        block.is_none(),
        "Report has no scan-bound fallback: {block:?}"
    );
}
