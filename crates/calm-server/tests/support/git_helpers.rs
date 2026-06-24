use std::path::Path;
use std::process::Command;

pub fn init_bare_origin(origin: &Path, seed: &Path) {
    run_git_no_cwd(["init", "--bare", path_str(origin)]);
    std::fs::create_dir_all(seed).expect("create seed repo");
    run_git(seed, ["init"]);
    run_git(
        seed,
        ["config", "user.email", "forge-workflow@example.test"],
    );
    run_git(seed, ["config", "user.name", "Forge Workflow Test"]);
    run_git(seed, ["branch", "-M", "main"]);
    std::fs::write(seed.join("README.md"), "initial\n").expect("write README");
    run_git(seed, ["add", "README.md"]);
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
