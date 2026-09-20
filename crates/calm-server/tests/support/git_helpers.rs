use std::path::{Path, PathBuf};
use std::process::Command;

pub fn init_bare_origin(origin: &Path, seed: &Path) {
    init_bare_origin_with_files(origin, seed, &[("README.md", "initial\n".to_string())]);
}

/// Seed the bare origin with a real Rust micro-crate and a hermetic `e2e-gate.sh` that runs its
/// tests via direct `rustc`; deliberately NO `Cargo.toml`, so nothing invokes cargo.
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

/// The gate cmd patched into the git-forge template descriptor in place of the production `cargo test`.
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

/// The seeded hermetic gate script. The kernel gate wrapper runs env-cleared, so PATH is pinned and
/// rustc is an absolute path; the output binary is pid-suffixed because concurrent gates share `tracks.cwd`.
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

/// Absolute path to the real toolchain `rustc` (`{sysroot}/bin/rustc`), HOME-independent so it works under the gate wrapper's cleared env.
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

/// Replicate the task-verify gate wrapper's conditions — `/bin/sh` with a fully cleared environment — and fail fast if `rustc` cannot run there.
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

/// A real Git work tree at a stable, name-derived path, for fixtures that need an attached track.
/// Concurrent first use is expected: the repo is built aside and its `.git` renamed into place, so only the winner's rename lands.
pub fn attached_repo_fixture(name: &str) -> String {
    let root = fixture_root();
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap_or_else(|e| panic!("create {path:?}: {e}"));
    if !is_git_work_tree(&path) {
        // The staging path must be unique per *call*: under `cargo test` one pid runs every test, so pid alone collides across threads.
        static NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nonce = NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let staging = root.join(format!(".init-{name}-{}-{nonce}", std::process::id()));
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging).unwrap_or_else(|e| panic!("create {staging:?}: {e}"));
        run_git(&staging, ["init", "-b", "main"]);
        // Losing this rename is expected for every process but the first; the error is kept because it is
        // the most informative fact when the assertion below fires.
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

/// The root [`attached_repo_fixture`] builds under: one directory per *run*, keyed by `NEXTEST_RUN_ID`.
/// `$TMPDIR` on the CI runner outlives the job, and its cleanup leaves hollow `.git` trees that a later run must never read.
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

/// Best effort: per-run roots would otherwise accumulate forever on the persistent runner. The age threshold keeps a live run's root out of reach.
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

/// Everything the failed [`attached_repo_fixture`] assertion needs to name the step that broke: git's own words plus what is on disk.
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

/// Asked of git, not the filesystem: a half-populated `.git` satisfies `is_dir()` but git refuses it.
/// The repository-redirecting env is scrubbed, and the answer must be *this* directory's own `.git` (rev-parse walks upward).
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
