use super::*;

#[test]
fn isolation_none_missing_owned_root_resolves_symlinked_ancestor() {
    let tmp = tempfile::tempdir().unwrap();
    let physical = tmp.path().join("physical");
    std::fs::create_dir(&physical).unwrap();
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&physical, &link).unwrap();
    let missing = link.join("missing/root");
    assert!(paths_overlap(&PathLocation::existing(&physical).unwrap(), &missing, true).unwrap());
    assert!(
        paths_overlap(
            &PathLocation::allow_missing(&physical.join("missing/root/child")).unwrap(),
            &missing,
            false
        )
        .unwrap()
    );
    assert!(
        !paths_overlap(
            &PathLocation::allow_missing(&physical.join("sibling")).unwrap(),
            &missing,
            true
        )
        .unwrap()
    );
    assert!(
        AttachedInheritedPath::new(
            physical.join("missing/root/child").to_str().unwrap().into(),
            &missing
        )
        .is_err()
    );
}
#[tokio::test]
async fn isolation_none_missing_cwd_cannot_use_ancestor_fallback() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = super::super::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let mut tx = super::super::begin_immediate_tx(repo.db()).await.unwrap();
    let error = admit_attached_workspace_tx(
        &mut tx,
        tmp.path().join("missing").to_str().unwrap(),
        &[tmp.path().join("managed")],
        &[],
        &TrackWorkspacePlan::AttachedFromCwd,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            &error,
            CalmError::Core(calm_types::error::CoreError::BadRequest(_))
        ),
        "{error}"
    );
}

#[tokio::test]
async fn isolation_none_unresolvable_owned_symlink_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let candidate = tmp.path().join("physical");
    std::fs::create_dir(&candidate).unwrap();
    let alias = tmp.path().join("alias");
    std::os::unix::fs::symlink(candidate.join("missing"), &alias).unwrap();
    let repo = super::super::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let mut tx = super::super::begin_immediate_tx(repo.db()).await.unwrap();
    let error = admit_attached_workspace_tx(
        &mut tx,
        candidate.to_str().unwrap(),
        &[alias.join("owned-root")],
        &[],
        &TrackWorkspacePlan::AttachedFromCwd,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            &error,
            CalmError::Core(calm_types::error::CoreError::BadRequest(_))
        ),
        "{error}"
    );
}

#[tokio::test]
async fn isolation_none_resolution_error_preserves_bad_request_message() {
    let tmp = tempfile::tempdir().unwrap();
    let candidate = tmp.path().join("candidate");
    std::fs::create_dir(&candidate).unwrap();
    let alias = tmp.path().join("alias");
    std::os::unix::fs::symlink(tmp.path().join("missing"), &alias).unwrap();
    let owned = alias.join("owned");
    let repo = super::super::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    let mut tx = super::super::begin_immediate_tx(repo.db()).await.unwrap();
    let error = admit_attached_workspace_tx(
        &mut tx,
        candidate.to_str().unwrap(),
        std::slice::from_ref(&owned),
        &[],
        &TrackWorkspacePlan::AttachedFromCwd,
    )
    .await
    .unwrap_err();
    let CalmError::Core(calm_types::error::CoreError::BadRequest(message)) = error else {
        panic!("expected BadRequest")
    };
    assert_eq!(
        message,
        format!(
            "cannot resolve existing ancestor {} of {}",
            alias.display(),
            owned.display()
        )
    );
}
