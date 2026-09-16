//! #1704 S2 — `tracks.claude_permissions_policy`: the shared writer
//! (`track_update_tx`), every `TrackRow` reader that splices one of the two
//! column consts, and the root-resolving ceiling read.
//!
//! Everything here drives the production functions; no fixture re-implements
//! the walk or the decode.

use serde_json::json;

use super::track_claude_permissions_ceiling_read;
use super::track_tree::MAX_TRACK_TREE_DEPTH;
use super::{SqlxRepo, area_create_tx, track_create_tx, track_update_tx};
use crate::db::RepoRead;
use crate::model::{NewArea, NewTrack, RequestTheme, TrackPatch};
use calm_types::claude_permissions::ClaudePermissionsScope;

async fn seed_area(repo: &SqlxRepo) -> String {
    let mut tx = repo.pool().begin().await.unwrap();
    let area = area_create_tx(
        &mut tx,
        NewArea {
            name: "policy".into(),
            color: "#000".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    area.id.to_string()
}

/// Production track creation: the writer the `child-track` operation uses.
async fn seed_track(repo: &SqlxRepo, area_id: &str, title: &str) -> String {
    let mut tx = repo.pool().begin().await.unwrap();
    let track = track_create_tx(
        &mut tx,
        NewTrack {
            area_id: area_id.to_string().into(),
            title: title.into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        None,
        &crate::db::sqlite::TrackWorkspacePlan::AttachedFromCwd,
        None,
        repo.track_area_cache(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    track.id.to_string()
}

async fn link(repo: &SqlxRepo, child: &str, parent: &str) {
    sqlx::query("UPDATE tracks SET parent_track_id=?1 WHERE id=?2")
        .bind(parent)
        .bind(child)
        .execute(repo.pool())
        .await
        .unwrap();
}

async fn patch(
    repo: &SqlxRepo,
    track: &str,
    policy: Option<Option<ClaudePermissionsScope>>,
) -> crate::error::Result<()> {
    let mut tx = repo.pool().begin().await.unwrap();
    let outcome = track_update_tx(
        &mut tx,
        track,
        TrackPatch {
            claude_permissions_policy: policy,
            ..Default::default()
        },
    )
    .await;
    match outcome {
        Ok(_) => tx.commit().await.unwrap(),
        Err(_) => tx.rollback().await.unwrap(),
    }
    outcome.map(|_| ())
}

async fn column(repo: &SqlxRepo, track: &str) -> Option<String> {
    sqlx::query_scalar("SELECT claude_permissions_policy FROM tracks WHERE id=?1")
        .bind(track)
        .fetch_one(repo.pool())
        .await
        .unwrap()
}

async fn ceiling(
    repo: &SqlxRepo,
    track: &str,
) -> crate::error::Result<Option<ClaudePermissionsScope>> {
    let mut conn = repo.pool().acquire().await.unwrap();
    track_claude_permissions_ceiling_read(&mut conn, track).await
}

fn policy() -> ClaudePermissionsScope {
    ClaudePermissionsScope {
        edit: Some(vec!["src/**".into(), "tests/**".into()]),
        bash: Some(vec!["git".into(), "python3 -m unittest".into()]),
        deny: Some(vec!["git rebase".into()]),
    }
}

/// The column is born NULL, a `Some(Some)` patch writes it and EVERY
/// `TrackRow` reader (both column consts) reads it back, a title patch leaves
/// it alone, `Some(None)` clears it.
#[tokio::test]
async fn policy_round_trips_through_every_track_row_reader() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = seed_area(&repo).await;
    let root = seed_track(&repo, &area, "root").await;
    assert_eq!(column(&repo, &root).await, None);
    assert_eq!(
        repo.track_get(&root)
            .await
            .unwrap()
            .unwrap()
            .claude_permissions_policy,
        None
    );

    patch(&repo, &root, Some(Some(policy()))).await.unwrap();
    assert_eq!(
        column(&repo, &root).await.as_deref(),
        Some(
            r#"{"edit":["src/**","tests/**"],"bash":["git","python3 -m unittest"],"deny":["git rebase"]}"#
        )
    );
    // `TRACK_SELECT_COLUMNS`: track_get, tracks_by_area, tracks_window.
    let got = repo.track_get(&root).await.unwrap().unwrap();
    assert_eq!(got.claude_permissions_policy, Some(policy()));
    let by_area = repo.tracks_by_area(&area).await.unwrap();
    assert_eq!(by_area.len(), 1);
    assert_eq!(by_area[0].claude_permissions_policy, Some(policy()));
    let window = repo.tracks_window(Some(&area), None, None).await.unwrap();
    assert_eq!(window[0].claude_permissions_policy, Some(policy()));
    // `TRACK_SELECT_COLUMNS_W`: track_detail.
    let detail = repo.track_detail(&root).await.unwrap().unwrap();
    assert_eq!(detail.track.claude_permissions_policy, Some(policy()));
    // The wire carries the key (`null` when absent, the `recipe_id` rule).
    let wire = serde_json::to_value(&detail.track).unwrap();
    assert_eq!(
        wire["claude_permissions_policy"],
        json!({"edit":["src/**","tests/**"],"bash":["git","python3 -m unittest"],"deny":["git rebase"]})
    );

    // A patch that omits the field leaves it alone; the returned row (the
    // writer's own `TRACK_SELECT_COLUMNS` read) carries it too.
    let mut tx = repo.pool().begin().await.unwrap();
    let after_title = track_update_tx(
        &mut tx,
        &root,
        TrackPatch {
            title: Some("renamed".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(after_title.title, "renamed");
    assert_eq!(after_title.claude_permissions_policy, Some(policy()));
    assert_eq!(
        repo.track_get(&root)
            .await
            .unwrap()
            .unwrap()
            .claude_permissions_policy,
        Some(policy())
    );

    // A present null clears.
    patch(&repo, &root, Some(None)).await.unwrap();
    assert_eq!(column(&repo, &root).await, None);
    let cleared = repo.track_get(&root).await.unwrap().unwrap();
    assert_eq!(cleared.claude_permissions_policy, None);
    assert_eq!(
        serde_json::to_value(&cleared).unwrap()["claude_permissions_policy"],
        json!(null)
    );
    assert_eq!(
        repo.track_detail(&root)
            .await
            .unwrap()
            .unwrap()
            .track
            .claude_permissions_policy,
        None
    );
}

/// Root-only, enforced by the shared in-tx writer: a child PATCH is a
/// `Conflict` naming the root and the column stays untouched; the same patch
/// on the root succeeds.
#[tokio::test]
async fn policy_patch_on_a_child_is_refused_by_the_shared_writer() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = seed_area(&repo).await;
    let root = seed_track(&repo, &area, "root").await;
    let child = seed_track(&repo, &area, "child").await;
    link(&repo, &child, &root).await;

    let error = patch(&repo, &child, Some(Some(policy())))
        .await
        .unwrap_err();
    assert!(
        matches!(
            &error,
            crate::error::TruthError::Core(calm_types::error::CoreError::Conflict(_))
        ),
        "{error:?}"
    );
    let message = error.to_string();
    assert!(
        message.contains("claude_permissions_policy is tree-root-only")
            && message.contains(&format!("track {child} is a child of {root}")),
        "{message}"
    );
    assert_eq!(column(&repo, &child).await, None);
    // Clearing a child is refused the same way (the shape, not the value).
    let error = patch(&repo, &child, Some(None)).await.unwrap_err();
    assert!(error.to_string().contains("tree-root-only"), "{error}");
    assert_eq!(column(&repo, &child).await, None);

    patch(&repo, &root, Some(Some(policy()))).await.unwrap();
    assert_eq!(
        repo.track_get(&root)
            .await
            .unwrap()
            .unwrap()
            .claude_permissions_policy,
        Some(policy())
    );
}

/// The ceiling read resolves the ROOT: depth 0 (the root itself), 1 and 2
/// all see the root's policy, a child created AFTER the root's policy was
/// set too, and the child rows themselves stay NULL. A bare root is `None`;
/// a cycle is a `Conflict`, never "no ceiling".
#[tokio::test]
async fn ceiling_read_resolves_the_tree_root() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = seed_area(&repo).await;
    let root = seed_track(&repo, &area, "root").await;
    let child = seed_track(&repo, &area, "child").await;
    let grandchild = seed_track(&repo, &area, "grandchild").await;
    link(&repo, &child, &root).await;
    link(&repo, &grandchild, &child).await;

    for track in [&root, &child, &grandchild] {
        assert_eq!(ceiling(&repo, track).await.unwrap(), None, "{track}");
    }

    patch(&repo, &root, Some(Some(policy()))).await.unwrap();
    for track in [&root, &child, &grandchild] {
        assert_eq!(
            ceiling(&repo, track).await.unwrap(),
            Some(policy()),
            "{track}"
        );
    }
    // A child created after the root's policy was set resolves to it as well;
    // its own row (the raw column every `Track` read returns) is NULL.
    let late = seed_track(&repo, &area, "late").await;
    link(&repo, &late, &grandchild).await;
    assert_eq!(ceiling(&repo, &late).await.unwrap(), Some(policy()));
    for track in [&child, &grandchild, &late] {
        assert_eq!(column(&repo, track).await, None, "{track}");
        assert_eq!(
            repo.track_get(track)
                .await
                .unwrap()
                .unwrap()
                .claude_permissions_policy,
            None,
            "{track}"
        );
    }

    // Clearing the root clears the ceiling of the whole tree.
    patch(&repo, &root, Some(None)).await.unwrap();
    for track in [&root, &child, &grandchild, &late] {
        assert_eq!(ceiling(&repo, track).await.unwrap(), None, "{track}");
    }

    // Fail closed: a cycle, an over-deep chain and a missing track.
    let a = seed_track(&repo, &area, "a").await;
    let b = seed_track(&repo, &area, "b").await;
    link(&repo, &a, &b).await;
    link(&repo, &b, &a).await;
    let error = ceiling(&repo, &a).await.unwrap_err();
    assert!(
        matches!(
            &error,
            crate::error::TruthError::Core(calm_types::error::CoreError::Conflict(_))
        ) && error
            .to_string()
            .contains(&format!("root unresolved for {a}")),
        "{error:?}"
    );
    let mut chain = Vec::new();
    for index in 0..=(MAX_TRACK_TREE_DEPTH + 2) {
        chain.push(seed_track(&repo, &area, &format!("deep{index}")).await);
    }
    for index in 1..chain.len() {
        link(&repo, &chain[index], &chain[index - 1]).await;
    }
    let error = ceiling(&repo, chain.last().unwrap()).await.unwrap_err();
    assert!(error.to_string().contains("root unresolved"), "{error}");
    let error = ceiling(&repo, "no-such-track").await.unwrap_err();
    assert!(error.to_string().contains("root unresolved"), "{error}");
}

/// The stored value is decoded by the lenient derive: a key this binary does
/// not know (a row written by a newer one) still decodes, on the row reader
/// and on the ceiling read alike; a value the scope derive cannot decode
/// fails both, never reading as "no policy" (the derive is lenient about
/// unknown keys and, like any serde struct, accepts a positional array, so
/// the undecodable probe is an array of strings).
#[tokio::test]
async fn stored_policy_tolerates_unknown_keys_and_refuses_non_objects() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = seed_area(&repo).await;
    let root = seed_track(&repo, &area, "root").await;
    sqlx::query("UPDATE tracks SET claude_permissions_policy=?1 WHERE id=?2")
        .bind(r#"{"edit":["**"],"protect":["src/**"]}"#)
        .bind(&root)
        .execute(repo.pool())
        .await
        .unwrap();
    let expected = ClaudePermissionsScope {
        edit: Some(vec!["**".into()]),
        bash: None,
        deny: None,
    };
    assert_eq!(
        repo.track_get(&root)
            .await
            .unwrap()
            .unwrap()
            .claude_permissions_policy,
        Some(expected.clone())
    );
    assert_eq!(ceiling(&repo, &root).await.unwrap(), Some(expected));

    sqlx::query("UPDATE tracks SET claude_permissions_policy=?1 WHERE id=?2")
        .bind(r#"["**"]"#)
        .bind(&root)
        .execute(repo.pool())
        .await
        .unwrap();
    assert!(repo.track_get(&root).await.is_err(), "row decode fails");
    let error = ceiling(&repo, &root).await.unwrap_err();
    assert!(
        error.to_string().contains("does not decode as a scope"),
        "{error}"
    );
}

/// A patch that does not name the policy leaves the stored TEXT byte-for-byte:
/// the writer never re-serializes the column from its lenient row decode, so
/// a title patch by an older binary keeps a key only a newer one knows. A
/// patch that names the policy replaces the whole value.
#[tokio::test]
async fn a_patch_without_the_policy_keeps_unknown_keys_of_the_stored_value() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = seed_area(&repo).await;
    let root = seed_track(&repo, &area, "root").await;
    let stored = r#"{"edit":["**"],"future_key":1}"#;
    sqlx::query("UPDATE tracks SET claude_permissions_policy=?1 WHERE id=?2")
        .bind(stored)
        .bind(&root)
        .execute(repo.pool())
        .await
        .unwrap();

    let mut tx = repo.pool().begin().await.unwrap();
    let after_title = track_update_tx(
        &mut tx,
        &root,
        TrackPatch {
            title: Some("renamed".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(after_title.title, "renamed");
    let raw = column(&repo, &root).await.unwrap();
    assert_eq!(raw, stored, "the raw column text is untouched");
    assert!(raw.contains("future_key"));

    patch(&repo, &root, Some(Some(policy()))).await.unwrap();
    let raw = column(&repo, &root).await.unwrap();
    assert!(
        !raw.contains("future_key"),
        "a policy patch replaces it: {raw}"
    );
    assert_eq!(
        repo.track_get(&root)
            .await
            .unwrap()
            .unwrap()
            .claude_permissions_policy,
        Some(policy())
    );
}
