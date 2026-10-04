//! Capstone deterministic support gates: these run WITHOUT a codex binary (no skip).

use super::capstone::*;
use crate::support::codex_fixture::*;
use crate::support::gh_shim::{run_gh, seed_shim_issue_body, write_gh_shim};
use crate::support::git_helpers::*;
use calm_server::plugin_host::Manifest;
use calm_server::templates::ISSUE_DEVELOPMENT;
use serde_json::{Value, json};

/// The seeded gate script must pass under the task-verify wrapper's EXACT conditions (`/bin/sh`, cleared env, repo cwd) and be cargo-free.
#[test]
fn capstone_gate_script_is_hermetic_and_cargo_free() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let origin = tmp.path().join("origin.git");
    let clone = tmp.path().join("clone");
    seed_rust_micro_crate(&origin, &tmp.path().join("seed"));
    clone_for_track(&origin, &clone);

    let script = std::fs::read_to_string(clone.join("e2e-gate.sh")).expect("seeded gate script");
    assert!(
        !script.contains("cargo"),
        "seeded gate script must never invoke cargo:\n{script}"
    );
    assert!(!CAPSTONE_GATE_CMD.contains("cargo"));
    assert!(
        !clone.join("Cargo.toml").exists() && !clone.join("src/Cargo.toml").exists(),
        "the micro-crate must not carry a Cargo.toml (removes every cargo surface)"
    );

    let out = std::process::Command::new("/bin/sh")
        .arg("e2e-gate.sh")
        .current_dir(&clone)
        .env_clear()
        .output()
        .expect("run seeded gate script env-cleared");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "env-cleared gate script failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("test result: ok"),
        "gate script must run the seeded unit test\nstdout:\n{stdout}"
    );
}

/// Shipped git-forge `templates[]` is an id handle only; gate cmds live in report task blocks, not the descriptor.
#[test]
fn shipped_git_forge_templates_are_id_only() {
    let raw = std::fs::read_to_string(manifest_path()).expect("read git-forge manifest");
    let value: Value = serde_json::from_str(&raw).expect("manifest json");
    assert_eq!(
        value["templates"],
        json!([{ "id": "issue-development" }]),
        "S5 git-forge templates[] must be id-only"
    );
    let manifest = Manifest::parse(&raw).expect("production manifest parses");
    assert_eq!(manifest.templates.len(), 1);
    assert_eq!(manifest.templates[0].id, "issue-development");
}

/// The fixture origin names GitHub while git reaches the local bare repository: the configured URL
/// passes the template's repo cross-check, and fetch, push and the kernel's
/// `ls-remote --get-url` resolve to the local origin.
#[test]
fn fixture_origin_names_github_and_reaches_the_local_bare_repo() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let origin = tmp.path().join("origin.git");
    let clone = tmp.path().join("clone");
    init_bare_origin(&origin, &tmp.path().join("seed"));
    clone_for_track(&origin, &clone);
    point_origin_at_github(&clone, &origin, &fixture_github_url());

    assert_eq!(
        cross_checked_origin_repo(&issue_development_method(), &clone),
        issue_development_input(1)["repo"]
    );
    let local = origin.display().to_string();
    assert_eq!(git_stdout(&clone, ["remote", "get-url", "origin"]), local);
    assert_eq!(
        git_stdout(&clone, ["ls-remote", "--get-url", "origin"]),
        local
    );
    run_git(&clone, ["fetch", "origin"]);
    run_git(&clone, ["checkout", "-b", "fixture-origin-probe"]);
    stage_git_change(&clone, "PROBE.md", "probe\n");
    run_git(&clone, ["commit", "-m", "probe"]);
    run_git(&clone, ["push", "origin", "fixture-origin-probe"]);
    assert_eq!(
        git_stdout_no_cwd([
            "--git-dir",
            local.as_str(),
            "rev-parse",
            "fixture-origin-probe"
        ]),
        git_stdout(&clone, ["rev-parse", "HEAD"])
    );
}

/// A multi-valued `remote.origin.url` whose first value names another repository: the template's
/// repo cross-check reads the first value, so it reports that repository, not input.repo.
#[test]
fn repo_cross_check_reads_the_first_of_several_origin_urls() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let origin = tmp.path().join("origin.git");
    let clone = tmp.path().join("clone");
    init_bare_origin(&origin, &tmp.path().join("seed"));
    clone_for_track(&origin, &clone);
    run_git(
        &clone,
        [
            "remote",
            "set-url",
            "origin",
            "https://github.com/neige-e2e/other-repo.git",
        ],
    );
    let fixture_url = fixture_github_url();
    run_git(
        &clone,
        ["config", "--add", "remote.origin.url", fixture_url.as_str()],
    );

    let observed = cross_checked_origin_repo(&issue_development_method(), &clone);
    assert_eq!(observed, "neige-e2e/other-repo");
    assert_ne!(observed, issue_development_input(1)["repo"]);
}

/// The bound input the fixture writes passes the shipped git-forge input schema, through the check
/// the track binding re-runs at Planner start.
#[test]
fn issue_development_input_binds_against_the_shipped_manifest() {
    let manifest = read_manifest();
    calm_server::plugin_host::template_input::validate_template_input_binding(
        calm_server::plugin_host::template_input::TemplateInputOwner::Plugin(&manifest),
        Some(&issue_development_input(CAPSTONE_ISSUE_NUMBER)),
    )
    .expect("fixture template_input binds");
    assert_eq!(
        issue_development_input(CAPSTONE_ISSUE_NUMBER)["merge_policy"],
        "auto-merge"
    );
}

/// The fixture's Planner card carries the issue-development working method the create route
/// stores.
#[test]
fn template_planner_card_payload_carries_the_issue_development_method() {
    let payload = calm_server::routes::tracks::template_planner_card_payload_for_test(
        Some("goal".into()),
        calm_server::session_projection_repo::AgentProvider::Codex,
        ISSUE_DEVELOPMENT,
    )
    .expect("planner card payload");
    assert_eq!(
        payload["template_context"]["body"],
        json!(issue_development_method())
    );
    assert_eq!(payload["prompt"], "goal");
}

/// gh shim `issue view --json body`: a seeded per-issue body file wins; absent
/// a seeded file the historical hardcoded fallback is byte-preserved.
#[test]
fn gh_shim_issue_view_prefers_seeded_body_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write_gh_shim(tmp.path());
    let gh = tmp.path().join("gh");
    let repo = tmp.path().join("origin.git");
    let repo_arg = repo.display().to_string();
    seed_shim_issue_body(&repo, CAPSTONE_ISSUE_NUMBER, CAPSTONE_ISSUE_BODY);

    let seeded = run_gh(
        &gh,
        &[
            "issue",
            "view",
            &CAPSTONE_ISSUE_NUMBER.to_string(),
            "--repo",
            &repo_arg,
            "--json",
            "body",
            "--jq",
            ".body",
        ],
    );
    assert!(seeded.status.success());
    assert_eq!(
        String::from_utf8_lossy(&seeded.stdout),
        CAPSTONE_ISSUE_BODY,
        "seeded issue body file must be served verbatim"
    );

    let fallback = run_gh(
        &gh,
        &[
            "issue", "view", "9999", "--repo", &repo_arg, "--json", "body", "--jq", ".body",
        ],
    );
    assert!(fallback.status.success());
    assert_eq!(
        String::from_utf8_lossy(&fallback.stdout),
        "# Issue 9999\n\nFake issue body for issue-development ingestion.\n",
        "unseeded issues must keep the historical hardcoded body (behavior-preserving)"
    );
}

/// A child forked by another thread can hold a fork-inherited write fd to the shim until it execs, so a direct spawn can fail ETXTBSY; `run_gh` must retry. Linux-only: macOS does not enforce ETXTBSY.
#[cfg(target_os = "linux")]
#[test]
fn gh_shim_spawn_retries_transient_etxtbsy() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write_gh_shim(tmp.path());
    let gh = tmp.path().join("gh");
    let repo_arg = tmp.path().join("origin.git").display().to_string();
    let args = [
        "issue", "view", "1234", "--repo", &repo_arg, "--json", "body", "--jq", ".body",
    ];

    let held = std::fs::OpenOptions::new()
        .write(true)
        .open(&gh)
        .expect("open write fd on gh shim");

    // Repro gate: with the write fd held, the raw exec fails ETXTBSY.
    let raw_err = std::process::Command::new(&gh)
        .args(args)
        .output()
        .expect_err("raw spawn must fail while a write fd is held");
    assert_eq!(
        raw_err.kind(),
        std::io::ErrorKind::ExecutableFileBusy,
        "raw spawn under a held write fd must fail ETXTBSY, got: {raw_err}"
    );

    let releaser = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        drop(held);
    });

    let out = run_gh(&gh, &args);
    releaser.join().expect("join fd releaser thread");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "# Issue 1234\n\nFake issue body for issue-development ingestion.\n",
        "run_gh must succeed with the expected shim output once the fd is released"
    );
}
