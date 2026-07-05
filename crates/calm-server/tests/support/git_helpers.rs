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

/// The gate cmd the #840 capstone patches into the git-forge workflow
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
/// pid-suffixed: gates of concurrently-verifying tasks share `waves.cwd`.
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
        ["config", "user.email", "forge-workflow@example.test"],
    );
    run_git(seed, ["config", "user.name", "Forge Workflow Test"]);
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

pub fn clone_for_wave(origin: &Path, target: &Path) {
    run_git_no_cwd(["clone", path_str(origin), path_str(target)]);
    configure_repo_identity(target);
}

pub fn configure_repo_identity(repo: &Path) {
    run_git(
        repo,
        ["config", "user.email", "forge-workflow@example.test"],
    );
    run_git(repo, ["config", "user.name", "Forge Workflow Test"]);
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
