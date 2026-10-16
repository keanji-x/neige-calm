#[test]
fn a_half_built_workspace_of_ours_is_repaired() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (root, repo_root) = sandbox(&tmp);
    materialize(&root, &repo_root).unwrap();

    let marker = std::fs::read_to_string(repo_root.join(".git").join(super::OWNER_MARKER)).unwrap();
    std::fs::remove_dir_all(repo_root.join(".git")).unwrap();
    std::fs::create_dir_all(repo_root.join(".git")).unwrap();
    std::fs::write(repo_root.join(".git").join(super::OWNER_MARKER), marker).unwrap();
    assert!(!head_resolves(&repo_root), "fixture did not break the repo");

    materialize(&root, &repo_root).expect("a marked half-built workspace must be repaired");
    assert!(head_resolves(&repo_root));
}

#[test]
fn materialize_survives_hostile_git_environment_variables() {
    let tmp = tempfile::TempDir::new().unwrap();
    let poison = tmp.path().join("poison");
    std::fs::create_dir_all(&poison).unwrap();
    let env = GitEnv::set(&[
        ("LC_ALL", std::ffi::OsStr::new("C")),
        ("LANGUAGE", std::ffi::OsStr::new("")),
        ("GIT_DIR", poison.as_os_str()),
        ("GIT_WORK_TREE", poison.as_os_str()),
        ("GIT_INDEX_FILE", poison.as_os_str()),
        ("GIT_OBJECT_DIRECTORY", poison.as_os_str()),
        ("GIT_AUTHOR_DATE", std::ffi::OsStr::new("not-a-date")),
        ("GIT_COMMITTER_NAME", std::ffi::OsStr::new("")),
    ]);

    let control = tmp.path().join("control");
    std::fs::create_dir_all(&control).unwrap();
    let control_init = Command::new("git")
        .args(["-c", "init.templateDir=", "-c", "init.defaultBranch=main"])
        .arg("init")
        .arg(&control)
        .output()
        .unwrap();
    let control_commit = Command::new("git")
        .arg("-C")
        .arg(&control)
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.name=neige",
            "-c",
            "user.email=neige@localhost",
            "commit",
            "--allow-empty",
            "-m",
            "control",
        ])
        .output()
        .unwrap();
    assert!(
        !control_init.status.success() || !control_commit.status.success(),
        "the injected git environment did not break an un-isolated git; this \
         test would pass without any isolation"
    );

    let tmp2 = tempfile::TempDir::new().unwrap();
    let (root, repo_root) = sandbox(&tmp2);
    materialize(&root, &repo_root).expect("materialize under a hostile git environment");

    // Restore the environment before verifying, so the repository is judged by a clean `git`.
    drop(env);
    assert!(head_resolves(&repo_root));
    assert_worker_checkout_ready(&repo_root);
}

#[test]
fn a_stale_lock_file_from_a_killed_init_is_cleared() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let (root, repo_root) = sandbox(&tmp);

    std::fs::create_dir_all(repo_root.join(".git")).unwrap();
    std::fs::write(
        repo_root.join(".git").join(super::OWNER_MARKER),
        format!("{TRACK}\n"),
    )
    .unwrap();
    std::fs::write(repo_root.join(".git/config.lock"), "").unwrap();
    assert!(
        !head_resolves(&repo_root),
        "fixture must start un-materialized"
    );

    materialize(&root, &repo_root).expect("a stale lock of ours must be cleared, not reported");
    assert!(head_resolves(&repo_root));
    assert!(
        !repo_root.join(".git/config.lock").exists(),
        "the stale lock is still there; the next call will fail the same way"
    );

    materialize(&root, &repo_root).expect("second call");
}

#[test]
fn a_stale_ref_lock_is_cleared() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let (root, repo_root) = sandbox(&tmp);
    materialize_managed_workspace_inner(&root, &repo_root, TRACK, InitCommit::Skip).unwrap();
    std::fs::write(repo_root.join(".git/HEAD.lock"), "").unwrap();
    assert!(!head_resolves(&repo_root));

    materialize(&root, &repo_root).expect("a stale ref lock of ours must be cleared");
    assert!(head_resolves(&repo_root));
}

/// `GIT_TEMPLATE_DIR` outranks `-c init.templateDir=`; the init commit survives (`--no-verify`) and the hook fires later.
#[test]
fn a_template_dir_in_the_environment_cannot_inject_hooks() {
    let tmp = tempfile::TempDir::new().unwrap();
    let template = tmp.path().join("tmpl");
    std::fs::create_dir_all(template.join("hooks")).unwrap();
    let hook = template.join("hooks").join("pre-commit");
    std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let env = GitEnv::set(&[
        ("LC_ALL", std::ffi::OsStr::new("C")),
        ("LANGUAGE", std::ffi::OsStr::new("")),
        ("GIT_TEMPLATE_DIR", template.as_os_str()),
    ]);

    // Sanity: the injection works against an un-isolated git, so the test cannot pass on a git that ignores the variable.
    let control = tmp.path().join("control");
    assert!(
        Command::new("git")
            .args(["-c", "init.templateDir=", "-c", "init.defaultBranch=main"])
            .arg("init")
            .arg(&control)
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(
        control.join(".git/hooks/pre-commit").exists(),
        "GIT_TEMPLATE_DIR did not inject through `-c init.templateDir=` on this \
         git, so this test cannot detect the hole it is guarding"
    );

    let (root, repo_root) = sandbox(&tmp);
    materialize(&root, &repo_root).expect("materialize under an injected template dir");
    assert!(
        !repo_root.join(".git/hooks/pre-commit").exists(),
        "a hook was injected into a server-owned workspace; every later git \
         command a worker runs there would execute it"
    );

    drop(env);
    assert!(head_resolves(&repo_root));
}

/// KNOWN GAP: the symlink refusal happens after a complete repository was written at the resolved location.
#[test]
#[cfg(unix)]
fn n7_a_refused_symlink_workspace_leaves_an_orphan_repository_outside_the_root() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let (root, repo_root) = sandbox(&tmp);
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, repo_root.parent().unwrap()).unwrap();

    materialize(&root, &repo_root).expect_err("symlink must be refused");

    let orphan = elsewhere.join(TRACK);
    assert!(
        orphan.join(".git").is_dir() && orphan.join(".git").join(super::OWNER_MARKER).exists(),
        "KNOWN GAP (#1147 N7): if this assertion starts failing, the orphan is \
         being cleaned up — delete this test and note the fix, do not loosen it"
    );
}

/// KNOWN GAP: losing our own marker is an unconditional refusal with no re-claim path.
#[test]
fn n5_losing_our_own_marker_is_an_unrecoverable_refusal() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let (root, repo_root) = sandbox(&tmp);
    materialize(&root, &repo_root).unwrap();
    std::fs::remove_file(repo_root.join(".git").join(super::OWNER_MARKER)).unwrap();

    let error = materialize(&root, &repo_root)
        .expect_err("KNOWN GAP (#1147 N5): a marker-less workspace of ours is refused forever");
    assert!(
        error
            .to_string()
            .contains("carries no neige ownership marker"),
        "unexpected error: {error}"
    );
}

/// KNOWN GAP: moving the workspace root strands existing managed tracks with no migration path.
#[test]
fn n4_moving_the_workspace_root_strands_existing_tracks() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let (old_root, repo_root) = sandbox(&tmp);
    materialize(&old_root, &repo_root).unwrap();

    let new_root = tmp.path().join("moved-root");
    std::fs::create_dir_all(&new_root).unwrap();
    let error = materialize_managed_workspace(&new_root, &repo_root, TRACK).expect_err(
        "KNOWN GAP (#1147 N4): an existing track under the old root is refused \
         outright after the root moves, with no migration path",
    );
    assert!(
        error.to_string().contains("outside the managed"),
        "unexpected error: {error}"
    );
    assert!(
        error.to_string().contains("N4"),
        "the error must name the gap so the next reader is not left guessing \
         about a symlink that is not there: {error}"
    );
}

/// Thread-local so the armed observer cannot leak into another test sharing the process.
type CrashObserver = std::sync::Arc<std::sync::Mutex<Vec<String>>>;
thread_local! {
    static CRASH_OBSERVER: std::cell::RefCell<Option<CrashObserver>> =
        const { std::cell::RefCell::new(None) };
}

/// Records instead of panicking: a panic while the `RefCell` is borrowed would abort during unwind.
pub(super) fn claim_crash_point(path: &Path) {
    claim_park_point();
    let sink = CRASH_OBSERVER.with(|slot| slot.borrow().clone());
    let Some(sink) = sink else { return };
    if let Err(violation) = workspace_dir_is_empty_or_ours(path) {
        sink.lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(violation);
    }
}

struct CrashObserverGuard(CrashObserver);

impl CrashObserverGuard {
    fn arm() -> Self {
        let sink: CrashObserver = Default::default();
        CRASH_OBSERVER.with(|slot| *slot.borrow_mut() = Some(sink.clone()));
        Self(sink)
    }

    fn violations(&self) -> Vec<String> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl Drop for CrashObserverGuard {
    fn drop(&mut self) {
        CRASH_OBSERVER.with(|slot| *slot.borrow_mut() = None);
    }
}

/// The claim invariant: if `<path>` has any entry at all, it carries our marker.
fn workspace_dir_is_empty_or_ours(path: &Path) -> std::result::Result<(), String> {
    let entries: Vec<_> = match std::fs::read_dir(path) {
        Ok(read) => read
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<std::result::Result<_, _>>()
            .map_err(|error| format!("read_dir({}): {error}", path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("read_dir({}): {error}", path.display())),
    };
    if entries.is_empty() {
        return Ok(());
    }
    match std::fs::read_to_string(path.join(".git").join(super::OWNER_MARKER)) {
        Ok(contents) if contents.trim() == TRACK => Ok(()),
        Ok(contents) => Err(format!(
            "{} holds {entries:?} but the marker reads {contents:?}, not `{TRACK}` \
             — the foreign-owner arm refuses this forever",
            path.display()
        )),
        Err(error) => Err(format!(
            "{} holds {entries:?} and has no readable marker ({error}) \
             — `None if dir_has_entries` refuses this forever",
            path.display()
        )),
    }
}

/// The window is not raced for: `claim_crash_point` fires at each point where the claim changed what
/// `read_dir(<path>)` reports, and the observer checks the invariant there.
#[test]
fn the_ownership_claim_never_leaves_an_unmarked_non_empty_workspace() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let (root, repo_root) = sandbox(&tmp);

    let observer = CrashObserverGuard::arm();
    materialize(&root, &repo_root).expect("materialize");
    let violations = observer.violations();

    assert!(
        violations.is_empty(),
        "the ownership claim passed through {} state(s) that the fence would \
         refuse forever:\n{}",
        violations.len(),
        violations.join("\n")
    );
}

/// Asserted through the mechanism: a rewrite must replace the marker's inode, which an in-place truncating write cannot.
#[test]
fn republishing_the_marker_replaces_the_inode_instead_of_truncating_it() {
    use std::os::unix::fs::MetadataExt;

    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let (root, repo_root) = sandbox(&tmp);
    materialize(&root, &repo_root).unwrap();

    let marker = repo_root.join(".git").join(super::OWNER_MARKER);
    let before = std::fs::metadata(&marker).unwrap().ino();

    // Break `HEAD` without touching the marker, so re-materialize re-asserts the marker.
    std::fs::remove_file(repo_root.join(".git").join("HEAD")).unwrap();
    assert!(!head_resolves(&repo_root), "fixture did not break HEAD");

    materialize(&root, &repo_root).expect("re-materialize repairs the workspace");

    let after = std::fs::metadata(&marker).unwrap().ino();
    assert_ne!(
        before, after,
        "the marker was rewritten in place at inode {before}: a crash inside \
         that write leaves a torn marker at the published path"
    );
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap().trim(),
        TRACK,
        "the republished marker must still name this track"
    );
}

/// A directory whose only entry is `.git/` stays refused: there is no marker-absence allowlist.
#[test]
fn a_dot_git_only_directory_without_a_marker_is_still_refused() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let (root, repo_root) = sandbox(&tmp);
    std::fs::create_dir_all(repo_root.join(".git")).unwrap();

    let error = materialize(&root, &repo_root)
        .expect_err("an unmarked directory containing only `.git/` must stay refused");
    assert!(
        error.to_string().contains("refusing to reuse"),
        "unexpected error: {error}"
    );
}

/// Bound on every wait below, the park included: a parked thread that is never released resumes on its own
/// and fails on the test's own assertions instead of wedging the runner.
const PARK_BOUND: std::time::Duration = std::time::Duration::from_secs(20);

/// A rendezvous armed on one thread, parking it at the `at`-th [`claim_crash_point`] call.
struct ClaimPark {
    at: usize,
    seen: usize,
    arrived: std::sync::mpsc::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
}

thread_local! {
    static CLAIM_PARK: std::cell::RefCell<Option<ClaimPark>> =
        const { std::cell::RefCell::new(None) };
}

fn arm_park(at: usize) -> (std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>) {
    let (arrived_tx, arrived_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    CLAIM_PARK.with(|slot| {
        *slot.borrow_mut() = Some(ClaimPark {
            at,
            seen: 0,
            arrived: arrived_tx,
            release: release_rx,
        })
    });
    (release_tx, arrived_rx)
}

/// The armed rendezvous is `take`n when it fires, so the borrow is released before the wait.
fn claim_park_point() {
    let park = CLAIM_PARK.with(|slot| {
        let mut slot = slot.borrow_mut();
        let park = slot.as_mut()?;
        park.seen += 1;
        if park.seen == park.at {
            slot.take()
        } else {
            None
        }
    });
    let Some(park) = park else { return };
    let _ = park.arrived.send(());
    let _ = park.release.recv_timeout(PARK_BOUND);
}

/// A second claimer for `path` calling the real claim directly — the per-path mutex lives in the caller,
/// so this is exactly a second *process*: no shared mutex, one shared area directory.
fn spawn_parked_claim(
    path: std::path::PathBuf,
    at: usize,
) -> (
    std::sync::mpsc::Sender<()>,
    std::sync::mpsc::Receiver<()>,
    std::thread::JoinHandle<crate::error::Result<()>>,
) {
    let (handback_tx, handback_rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        handback_tx
            .send(arm_park(at))
            .expect("hand the rendezvous back");
        super::claim_owner_marker(&path, TRACK)
    });
    let (release_tx, arrived_rx) = handback_rx
        .recv_timeout(PARK_BOUND)
        .expect("the claimer thread must hand its rendezvous back");
    (release_tx, arrived_rx, handle)
}

/// Every staging directory beside `path`; the per-attempt suffix means a test cannot compute the name.
fn staging_dirs(path: &Path) -> Vec<std::path::PathBuf> {
    let parent = path.parent().expect("the workspace path has a parent");
    let mut found: Vec<_> = std::fs::read_dir(parent)
        .expect("read the area directory")
        .map(|entry| entry.expect("read an area entry").path())
        .filter(|entry| {
            entry
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(super::CLAIM_STAGING_PREFIX))
        })
        .collect();
    found.sort();
    found
}

fn only_staging_dir(path: &Path) -> std::path::PathBuf {
    let mut found = staging_dirs(path);
    assert_eq!(
        found.len(),
        1,
        "expected exactly one staging directory beside {}, found {found:?}",
        path.display()
    );
    found.pop().unwrap()
}

#[test]
fn a_claim_that_loses_the_staging_race_fails_closed_onto_the_winners_marker() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let (root, path) = sandbox(&tmp);
    std::fs::create_dir_all(&path).unwrap();

    let (release, arrived, loser) = spawn_parked_claim(path.clone(), 1);
    arrived
        .recv_timeout(PARK_BOUND)
        .expect("the loser must reach its first crash point");
    let losers_staging = only_staging_dir(&path);

    super::claim_owner_marker(&path, TRACK).expect("the winner publishes its claim");
    assert_eq!(
        std::fs::read_to_string(super::owner_marker_path(&path))
            .expect("the winner's marker")
            .trim(),
        TRACK,
        "premise: the winner published a correctly marked workspace"
    );
    assert!(
        losers_staging.exists(),
        "premise: the winner left the loser's staging alone — the staging name is \
         per attempt since #1458"
    );

    let _ = release.send(());
    let error = loser
        .join()
        .expect("the loser thread must not panic")
        .expect_err(
            "`<path>` is no longer empty when the loser gets there, so its publishing \
             rename must fail rather than clobber the winner's workspace",
        );
    assert!(
        error.to_string().contains("publish ownership claim"),
        "the refusal must come from the fail-closed publishing rename, not from \
         something incidental: {error}"
    );
    assert!(
        error.to_string().contains("Directory not empty"),
        "and specifically from `ENOTEMPTY`, which is what makes the rename \
         non-clobbering: {error}"
    );
    assert!(
        staging_dirs(&path).is_empty(),
        "the loser removes its own staging on the error path, so nothing is left \
         behind: {:?}",
        staging_dirs(&path)
    );

    workspace_dir_is_empty_or_ours(&path).expect(
        "the fence must still accept the workspace after the losing claim returned — \
         `remove_dir_all` is called on the caller's own staging directory and never on \
         `<path>`",
    );
    materialize(&root, &path).expect(
        "and the next materialization must succeed: it reads the winner's marker and takes \
         the `Some(owner) if owner == track_id` branch",
    );
    assert!(
        head_resolves(&path),
        "the recovered workspace is a real repository"
    );
}

/// Mutation that must redden it: drop the `-{claim_attempt_id()}` suffix from the staging name.
#[test]
fn a_concurrent_claim_cannot_make_its_peer_publish_an_unmarked_workspace() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let (root, path) = sandbox(&tmp);
    std::fs::create_dir_all(&path).unwrap();

    let (release_one, arrived_one, publisher) = spawn_parked_claim(path.clone(), 3);
    arrived_one
        .recv_timeout(PARK_BOUND)
        .expect("claimer 1 must reach the crash point before the rename");
    let staging_one = only_staging_dir(&path);
    let staged_marker = staging_one.join(".git").join(super::OWNER_MARKER);
    assert_eq!(
        std::fs::read_to_string(&staged_marker)
            .expect("claimer 1's staged marker")
            .trim(),
        TRACK,
        "premise: claimer 1 staged a complete, marked claim"
    );

    let (release_two, arrived_two, peer) = spawn_parked_claim(path.clone(), 1);
    arrived_two
        .recv_timeout(PARK_BOUND)
        .expect("claimer 2 must reach its first crash point");
    let staging_dirs_now = staging_dirs(&path);
    assert_eq!(
        staging_dirs_now.len(),
        2,
        "the two claimers must be assembling under two different names: {staging_dirs_now:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&staged_marker)
            .expect("claimer 1's staged marker must have survived claimer 2's entry")
            .trim(),
        TRACK,
        "claimer 2 must not be able to remove or recreate claimer 1's staging"
    );

    let _ = release_one.send(());
    publisher
        .join()
        .expect("claimer 1 must not panic")
        .expect("the publishing rename succeeds and the claim reports success");

    workspace_dir_is_empty_or_ours(&path).expect(
        "inverted by #1458: `<path>` now carries this track's marker. Before the fix \
         this call returned the `no readable marker` violation",
    );

    let _ = release_two.send(());
    let error = peer
        .join()
        .expect("claimer 2 must not panic")
        .expect_err("`<path>` is no longer empty, so claimer 2's publishing rename must fail");
    assert!(
        error.to_string().contains("publish ownership claim")
            && error.to_string().contains("Directory not empty"),
        "claimer 2 must fail closed on `ENOTEMPTY` rather than clobber the published \
         workspace: {error}"
    );
    assert!(
        staging_dirs(&path).is_empty(),
        "and it must clean its own staging up: {:?}",
        staging_dirs(&path)
    );

    materialize(&root, &path).expect(
        "inverted by #1458: the fence accepts the workspace the race published. Before \
         the fix this refused with `is not empty and carries no neige ownership marker` \
         for every key, forever",
    );
    assert!(
        head_resolves(&path),
        "and the published workspace materializes into a real repository"
    );
}

