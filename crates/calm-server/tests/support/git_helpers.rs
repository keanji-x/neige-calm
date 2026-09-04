use std::path::{Path, PathBuf};
use std::process::Command;

pub fn init_bare_origin(origin: &Path, seed: &Path) {
    init_bare_origin_with_files(origin, seed, &[("README.md", "initial\n".to_string())]);
}

/// #840 capstone (P2): seed the bare origin with a REAL (non-toy) Rust
/// micro-crate: `src/lib.rs` with one existing `pub fn` + a passing `#[test]`,
/// and a hermetic `e2e-gate.sh` that compiles-and-runs the crate's unit tests
/// with a direct `rustc` invocation. Deliberately NO `Cargo.toml` anywhere —
/// that removes every cargo invocation surface (gate AND worker shell), the
/// #863-B recursive-suite amplifier. `RUSTC_WRAPPER`/sccache is cargo-mediated,
/// so direct rustc is immune to the sandbox sccache flake.
///
/// Fixture-boot preflight (#840 capstone pin d): the kernel's task-verify gate
/// wrapper runs `/bin/sh` with a CLEARED environment (task_verify_adapter
/// `env_clear()`), so this fails fast at seed time if the baked rustc cannot
/// run under those exact conditions.
pub fn seed_rust_micro_crate(origin: &Path, seed: &Path) {
    let rustc = resolve_hermetic_rustc();
    preflight_env_cleared_rustc(&rustc);
    init_bare_origin_with_files(
        origin,
        seed,
        &[
            ("src/lib.rs", RUST_MICRO_CRATE_LIB.to_string()),
            ("e2e-gate.sh", capstone_gate_script(&rustc)),
        ],
    );
}

/// The gate cmd the #840 capstone patches into the git-forge template
/// descriptor in place of the production `cargo test` (design P1).
pub const CAPSTONE_GATE_CMD: &str = "sh ./e2e-gate.sh";

const RUST_MICRO_CRATE_LIB: &str = r#"/// Greets `name`.
pub fn greet(name: &str) -> String {
    format!("Hello, {name}!")
}

#[cfg(test)]
mod tests {
    use super::greet;

    #[test]
    fn greet_includes_name() {
        assert_eq!(greet("neige"), "Hello, neige!");
    }
}
"#;

/// The seeded hermetic gate script. The kernel gate wrapper runs env-cleared,
/// so PATH is pinned here (linker discovery for `rustc --test`) and rustc is a
/// baked absolute toolchain path (a `~/.cargo/bin` rustup shim would need
/// `$HOME`, which the cleared env does not have). The output binary is
/// pid-suffixed: gates of concurrently-verifying tasks share `tracks.cwd`.
fn capstone_gate_script(rustc: &Path) -> String {
    format!(
        "#!/bin/sh\n\
         # Hermetic #840 capstone gate: compile-and-run this crate's unit tests\n\
         # with a direct rustc invocation only (#863-B amplifier defusal).\n\
         set -eu\n\
         PATH=/usr/bin:/bin\n\
         export PATH\n\
         out=\".gate-bin.$$\"\n\
         trap 'rm -f \"$out\"' EXIT\n\
         '{rustc}' --edition 2021 --test src/lib.rs -o \"$out\"\n\
         \"./$out\"\n",
        rustc = rustc.display()
    )
}

/// Absolute path to the real toolchain `rustc` (HOME-independent):
/// `{sysroot}/bin/rustc`. Resolved with the test process's full env; the
/// resolved binary itself then works under the gate wrapper's cleared env.
pub fn resolve_hermetic_rustc() -> PathBuf {
    let out = Command::new("rustc")
        .args(["--print", "sysroot"])
        .output()
        .expect("run `rustc --print sysroot` (rustc must be on PATH to seed the capstone gate)");
    assert!(
        out.status.success(),
        "`rustc --print sysroot` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let sysroot = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let rustc = Path::new(&sysroot).join("bin").join("rustc");
    assert!(
        rustc.is_file(),
        "toolchain rustc not found at {}",
        rustc.display()
    );
    rustc
}

/// #840 capstone pin (d): replicate the task-verify gate wrapper's execution
/// conditions — `/bin/sh` with a fully CLEARED environment — and fail fast if
/// `rustc` cannot even print its version there.
pub fn preflight_env_cleared_rustc(rustc: &Path) {
    let out = Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("'{}' --version", rustc.display()))
        .env_clear()
        .output()
        .expect("spawn env-cleared rustc preflight");
    assert!(
        out.status.success(),
        "env-cleared gate preflight: `{} --version` failed under /bin/sh with a \
         cleared environment (the task-verify wrapper runs exactly like this); \
         stdout:\n{}\nstderr:\n{}",
        rustc.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn init_bare_origin_with_files(origin: &Path, seed: &Path, files: &[(&str, String)]) {
    run_git_no_cwd(["init", "--bare", path_str(origin)]);
    std::fs::create_dir_all(seed).expect("create seed repo");
    run_git(seed, ["init"]);
    run_git(
        seed,
        ["config", "user.email", "forge-template@example.test"],
    );
    run_git(seed, ["config", "user.name", "Forge Template Test"]);
    run_git(seed, ["branch", "-M", "main"]);
    for (name, contents) in files {
        let path = seed.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create seed subdir");
        }
        std::fs::write(&path, contents).expect("write seed file");
        run_git(seed, ["add", *name]);
    }
    run_git(seed, ["commit", "-m", "initial"]);
    run_git(seed, ["remote", "add", "origin", path_str(origin)]);
    run_git(seed, ["push", "-u", "origin", "main"]);
    run_git_no_cwd([
        "--git-dir",
        path_str(origin),
        "symbolic-ref",
        "HEAD",
        "refs/heads/main",
    ]);
}

pub fn clone_for_track(origin: &Path, target: &Path) {
    run_git_no_cwd(["clone", path_str(origin), path_str(target)]);
    configure_repo_identity(target);
}

pub fn configure_repo_identity(repo: &Path) {
    run_git(
        repo,
        ["config", "user.email", "forge-template@example.test"],
    );
    run_git(repo, ["config", "user.name", "Forge Template Test"]);
}

pub fn stage_git_change(repo: &Path, name: &str, contents: &str) {
    std::fs::write(repo.join(name), contents).expect("write git change");
    run_git(repo, ["add", name]);
}

pub fn run_git<const N: usize>(repo: &Path, args: [&str; N]) {
    run_git_inner(Some(repo), args);
}

pub fn run_git_no_cwd<const N: usize>(args: [&str; N]) {
    run_git_inner(None, args);
}

pub fn run_git_capture<const N: usize>(repo: &Path, args: [&str; N]) -> String {
    let output = run_git_output(Some(repo), args);
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

pub fn git_ref_exists(repo: &Path, ref_name: &str) -> bool {
    run_git_output(Some(repo), ["show-ref", "--verify", "--quiet", ref_name])
        .status
        .success()
}

pub fn run_git_inner<const N: usize>(repo: Option<&Path>, args: [&str; N]) {
    let output = run_git_output(repo, args);
    assert!(
        output.status.success(),
        "git {:?} failed\nstdout:\n{}\nstderr:\n{}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

pub fn run_git_output<const N: usize>(
    repo: Option<&Path>,
    args: [&str; N],
) -> std::process::Output {
    let mut cmd = Command::new("git");
    cmd.args(args);
    if let Some(repo) = repo {
        cmd.current_dir(repo);
    }
    cmd.output().expect("run git")
}

fn path_str(path: &Path) -> &str {
    path.to_str().expect("test paths are utf-8")
}

pub fn git_stdout_no_cwd<const N: usize>(args: [&str; N]) -> String {
    let output = run_git_output(None, args);
    assert!(
        output.status.success(),
        "git {:?} failed\nstdout:\n{}\nstderr:\n{}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

pub fn git_stdout<const N: usize>(repo: &Path, args: [&str; N]) -> String {
    let output = run_git_output(Some(repo), args);
    assert!(
        output.status.success(),
        "git {:?} failed in {}\nstdout:\n{}\nstderr:\n{}",
        args,
        repo.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

pub fn is_hex_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// #1147 S3 — a real Git work tree at a stable, name-derived path, for
/// fixtures that need an **attached** track and do not care where it points.
///
/// `POST /api/tracks` now validates an attached `cwd` (absolute, exists, is a
/// Git work tree) instead of accepting any string, because the FE entry point
/// this slice adds is the first way a user can name one — and a path that only
/// fails later, as a worker's `spawn-failed`, is the defect #1147 was opened
/// on. Dozens of fixtures predate that check and pass literals like
/// `/tmp/issue-250-pr2-test`, which were never valid workspaces; this makes
/// them what they always claimed to be.
///
/// Idempotent and safe to share *within one run*: the directory is keyed by
/// `name` under [`fixture_root`], and re-init is a no-op, which matches how
/// those literals were already shared across tests within a file.
///
/// Sharing has to survive *concurrent* first use, not just repeated use. The
/// literals this replaces were shared across tests, and nextest runs every
/// test in its own process with several binaries in flight at once, so two
/// processes reach the un-initialized branch for the same key at the same
/// time. Two `git init`s in one directory race on `.git/config`'s lock and one
/// of them dies with `不能锁定配置文件 … 文件已存在`. So the repository is built
/// off to the side and its `.git` is *renamed* into place: the winner's rename
/// is atomic, the loser's fails because the destination is a non-empty
/// directory, and the loser's repository is discarded — the winner's is
/// identical, and no caller ever observes a half-initialized `.git`.
///
/// That protocol is only sound while the destination can be nothing but a
/// peer's freshly built `.git`. Making the root per-run (#1433, see
/// [`fixture_root`]) is what keeps that true.
pub fn attached_repo_fixture(name: &str) -> String {
    let root = fixture_root();
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap_or_else(|e| panic!("create {path:?}: {e}"));
    if !is_git_work_tree(&path) {
        // The staging path must be unique per *call*, not per process. Under
        // `cargo test`'s thread model one pid runs every test in a binary, so a
        // pid-only name collides whenever two threads reach this branch for the
        // same `name` — and each would then `remove_dir_all` the other's
        // staging mid-`git init`. pid + a monotonic counter is unique across
        // both models: the counter separates threads within a process, the pid
        // separates nextest's processes.
        static NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nonce = NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let staging = root.join(format!(".init-{name}-{}-{nonce}", std::process::id()));
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging).unwrap_or_else(|e| panic!("create {staging:?}: {e}"));
        run_git(&staging, ["init", "-b", "main"]);
        // Losing this rename is the expected outcome for every process but the
        // first; the assertion that matters is made below, after the race. The
        // error is kept rather than dropped: when the assertion does fire it is
        // the single most informative fact about why (#1433 was an `ENOTEMPTY`
        // here, invisible for a day behind a `let _ =`).
        let renamed = std::fs::rename(staging.join(".git"), path.join(".git"));
        let _ = std::fs::remove_dir_all(&staging);
        assert!(
            is_git_work_tree(&path),
            "attached_repo_fixture({name}): {path:?} is not a Git work tree after init\n\
             rename(staging/.git -> {path:?}/.git) = {renamed:?}\n{}",
            work_tree_diagnosis(&path)
        );
    }
    path.to_string_lossy().into_owned()
}

/// The root [`attached_repo_fixture`] builds under: one directory per *run*,
/// not one directory per `$TMPDIR`.
///
/// `$TMPDIR` is not scratch space on the self-hosted CI runner. The workflow
/// points it at `RUNNER_TEMP`, which outlives the job and is swept by the
/// runner's own post-job cleanup — a sweep that deletes every *file*
/// underneath and leaves the *directory tree* standing. Measured on the
/// `neige-calm-main` runner on 2026-09-04: 0 files and 1407 directories left
/// under `_work/_temp`, and 102 of the 103 `neige-attached-fixtures/<name>`
/// entries holding a `.git` made only of `branches/ hooks/ info/ objects/
/// refs/` — no `HEAD`, no `config`.
///
/// A later run then finds `<name>/.git` present but hollow. `git rev-parse`
/// refuses it, so the init branch runs, and renaming the freshly built `.git`
/// onto that husk fails with `ENOTEMPTY` — for every process, in every
/// subsequent run, until someone deletes the directory by hand. That is
/// #1433: `main` red all day, ~74 fixtures failing at once from the first
/// test that touched one, with nothing actually racing.
///
/// Keying the root by run is what removes it: a run only ever reads
/// directories it created itself. `NEXTEST_RUN_ID` is a single UUID for a
/// whole nextest run, exported into every test process nextest spawns, which
/// is exactly the sharing scope these fixtures need — same key inside a run,
/// never the same key across runs. Under a plain `cargo test` there is no run
/// id; a per-process root is used instead, which costs nothing because the
/// fixtures are only ever shared inside one test binary.
fn fixture_root() -> PathBuf {
    let base = std::env::temp_dir().join("neige-attached-fixtures");
    let token = std::env::var("NEXTEST_RUN_ID")
        .ok()
        .filter(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        .unwrap_or_else(|| format!("pid-{}", std::process::id()));
    let root = base.join(token);
    static SWEPT: std::sync::Once = std::sync::Once::new();
    SWEPT.call_once(|| sweep_finished_runs(&base, &root));
    root
}

/// Per-run roots would otherwise accumulate forever on the persistent runner
/// (the cleanup above empties files but keeps directories, so nothing else
/// ever reclaims them). Best effort by design: a failure here must not fail a
/// test, and a concurrent sweeper removing the same directory is fine.
///
/// The age threshold is what keeps a *live* run's root out of reach. A root
/// is written to whenever its run builds another fixture, and the Rust suite
/// this serves runs for minutes, not hours.
fn sweep_finished_runs(base: &Path, keep: &Path) {
    const FINISHED_RUN_AGE: std::time::Duration = std::time::Duration::from_secs(2 * 60 * 60);
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        if path == keep {
            continue;
        }
        let finished = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > FINISHED_RUN_AGE);
        if finished {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

/// Everything the failed [`attached_repo_fixture`] assertion needs to name the
/// step that broke, in one string: git's own words plus what is on disk.
///
/// The message it feeds used to say only "is not a Git work tree", which is
/// true of a lost race, of a hollow leftover, and of git refusing a directory
/// owned by another user alike — #1433 sat in `main` for a day partly because
/// the panic could not tell those apart.
fn work_tree_diagnosis(path: &Path) -> String {
    let output = rev_parse_git_dir(path);
    let mut report = format!(
        "git rev-parse --absolute-git-dir in {path:?}: {}\n  stdout: {}\n  stderr: {}\n",
        output.status,
        String::from_utf8_lossy(&output.stdout).trim(),
        String::from_utf8_lossy(&output.stderr).trim(),
    );
    for dir in [path.to_path_buf(), path.join(".git")] {
        report.push_str(&format!("  {dir:?}: {}\n", describe_dir(&dir)));
    }
    report
}

fn describe_dir(dir: &Path) -> String {
    match std::fs::read_dir(dir) {
        Err(err) => format!("unreadable ({err})"),
        Ok(entries) => {
            let mut names: Vec<String> = entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            format!("{} entries {names:?}", names.len())
        }
    }
}

/// "Does `path` already own a working repository?" — asked of git, not of the
/// filesystem.
///
/// The predicate gates whether the fixture gets (re)built, and what the callers
/// need is what the server's `validate_attached_workspace` will ask: can git
/// resolve a repository here. A `.git` directory left half-populated by an
/// interrupted init satisfies `is_dir()` while failing that, so the old
/// directory test would hand back a path the route then 400s on. Asking git
/// means such a leftover is *rejected* rather than trusted — #1433 is the
/// reminder that rejecting it is not the same as repairing it: nothing here
/// can rename a fresh `.git` onto a non-empty husk, which is why
/// [`fixture_root`] keeps husks out of a run's path in the first place.
///
/// Two refinements over a bare `rev-parse`:
///
///   * the repository-redirecting environment is scrubbed, the same set
///     `neige_git_command` removes — an inherited `GIT_DIR` would otherwise
///     make every path look initialized;
///   * the answer must be *this* directory's own `.git`. `rev-parse` walks
///     upward, so on a box whose `TMPDIR` happens to sit inside a repository a
///     bare success would skip the init and leave the fixtures sharing their
///     ancestor's repository.
fn is_git_work_tree(path: &Path) -> bool {
    let output = rev_parse_git_dir(path);
    if !output.status.success() {
        return false;
    }
    let git_dir = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    let own = path.join(".git");
    match (git_dir.canonicalize(), own.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// The one `git rev-parse` [`is_git_work_tree`] answers from, so that
/// [`work_tree_diagnosis`] reports on the same invocation rather than a
/// paraphrase of it.
fn rev_parse_git_dir(path: &Path) -> std::process::Output {
    const HOSTILE_GIT_ENV: [&str; 8] = [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_CEILING_DIRECTORIES",
        "GIT_TEMPLATE_DIR",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_SYSTEM",
        "GIT_CONFIG",
        "GIT_CONFIG_COUNT",
    ];
    let mut cmd = Command::new("git");
    for key in HOSTILE_GIT_ENV {
        cmd.env_remove(key);
    }
    cmd.current_dir(path)
        .args(["rev-parse", "--absolute-git-dir"])
        .output()
        .expect("run git")
}
