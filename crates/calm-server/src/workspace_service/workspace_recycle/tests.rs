//! Unit coverage for the recycle guards and the trash GC.

use std::path::{Path, PathBuf};

use super::*;
use crate::model::{TrackWorkspace, TrackWorkspaceKind};
use workspace::materialize::materialize_managed_workspace;

struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().join("workspaces");
        std::fs::create_dir_all(&root).unwrap();
        Fixture { _tmp: tmp, root }
    }

    /// Built by the production materializer, so the marker under test is the one production writes.
    fn managed(&self, area_id: &str, track_id: &str) -> PathBuf {
        let path = workspace::materialize::managed_workspace_path(&self.root, area_id, track_id);
        materialize_managed_workspace(&self.root, &path, track_id).unwrap();
        path
    }

    fn workspace(path: &Path, kind: TrackWorkspaceKind) -> TrackWorkspace {
        TrackWorkspace {
            kind,
            path: path.to_string_lossy().into_owned(),
            frozen_at: Some(1),
            worktree: None,
        }
    }

    fn recycle(&self, track_id: &str, workspace: &TrackWorkspace) -> RecycleDecision {
        recycle_track_workspace(
            &self.root,
            Some(AreaKind::User),
            track_id,
            workspace,
            1_000_000,
        )
        .unwrap()
    }
}

const OWNER_MARKER_RELATIVE: [&str; 2] = [".git", "neige-workspace"];
#[test]
fn an_attached_workspace_is_refused() {
    let f = Fixture::new();
    // Inside the root with a valid marker: only the typed kind makes this refuse.
    let path = f.managed("area-1", "track-1");
    let decision = f.recycle(
        "track-1",
        &Fixture::workspace(&path, TrackWorkspaceKind::Attached),
    );
    assert_eq!(decision.refusal(), Some(&RecycleRefusal::NotManaged));
    assert!(path.is_dir());
}

#[test]
fn a_system_area_workspace_is_refused() {
    let f = Fixture::new();
    let path = f.managed("area-sys", "launchpad-track");
    let decision = recycle_track_workspace(
        &f.root,
        Some(AreaKind::System),
        "launchpad-track",
        &Fixture::workspace(&path, TrackWorkspaceKind::Managed),
        1_000_000,
    )
    .unwrap();
    assert_eq!(decision.refusal(), Some(&RecycleRefusal::SystemArea));
    assert!(path.join(".git").is_dir());
}

#[test]
fn an_unknown_area_is_refused_rather_than_assumed_user() {
    let f = Fixture::new();
    let path = f.managed("area-1", "track-1");
    let decision = recycle_track_workspace(
        &f.root,
        None,
        "track-1",
        &Fixture::workspace(&path, TrackWorkspaceKind::Managed),
        1_000_000,
    )
    .unwrap();
    assert_eq!(decision.refusal(), Some(&RecycleRefusal::SystemArea));
    assert!(path.join(".git").is_dir());
}

#[test]
fn the_area_directory_is_removed_only_once_it_is_empty() {
    let f = Fixture::new();
    let one = f.managed("area-1", "track-1");
    let two = f.managed("area-1", "track-2");
    let ws_one = Fixture::workspace(&one, TrackWorkspaceKind::Managed);
    let ws_two = Fixture::workspace(&two, TrackWorkspaceKind::Managed);

    let mut partial = recycle_area_workspaces(
        &f.root,
        "area-1",
        Some(AreaKind::User),
        &[RecycleTarget {
            track_id: "track-1",
            workspace: &ws_one,
        }],
        1_000_000,
    )
    .unwrap();
    finalize_area_recycle(&f.root, "area-1", &mut partial);
    assert!(!partial.area_dir_removed);
    assert!(f.root.join("area-1").is_dir());
    assert!(two.join(".git").is_dir());

    let mut full = recycle_area_workspaces(
        &f.root,
        "area-1",
        Some(AreaKind::User),
        &[RecycleTarget {
            track_id: "track-2",
            workspace: &ws_two,
        }],
        1_000_001,
    )
    .unwrap();
    finalize_area_recycle(&f.root, "area-1", &mut full);
    assert!(full.area_dir_removed);
    assert!(!f.root.join("area-1").exists());
}

#[test]
fn a_area_directory_holding_an_unrecycled_track_is_kept() {
    let f = Fixture::new();
    let attached_shaped = f.managed("area-1", "track-1");
    let ws = Fixture::workspace(&attached_shaped, TrackWorkspaceKind::Attached);
    let mut report = recycle_area_workspaces(
        &f.root,
        "area-1",
        Some(AreaKind::User),
        &[RecycleTarget {
            track_id: "track-1",
            workspace: &ws,
        }],
        1_000_000,
    )
    .unwrap();
    finalize_area_recycle(&f.root, "area-1", &mut report);
    assert!(!report.area_dir_removed);
    assert!(attached_shaped.join(".git").is_dir());
}

#[test]
fn a_late_area_recycle_error_restores_every_earlier_workspace() {
    let f = Fixture::new();
    let first = f.managed("area-1", "track-1");
    let second = f.managed("area-1", "track-2");
    let marker = OWNER_MARKER_RELATIVE
        .iter()
        .fold(second.clone(), |path, part| path.join(part));
    std::fs::write(marker, "../escape").unwrap();
    let first_workspace = Fixture::workspace(&first, TrackWorkspaceKind::Managed);
    let second_workspace = Fixture::workspace(&second, TrackWorkspaceKind::Managed);

    let error = recycle_area_workspaces(
        &f.root,
        "area-1",
        Some(AreaKind::User),
        &[
            RecycleTarget {
                track_id: "track-1",
                workspace: &first_workspace,
            },
            RecycleTarget {
                track_id: "../escape",
                workspace: &second_workspace,
            },
        ],
        1_000_000,
    )
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("does not form a single path segment")
    );
    assert!(
        first.join(".git").is_dir(),
        "the first move was not restored"
    );
    assert!(second.join(".git").is_dir(), "the failing target was moved");
}
