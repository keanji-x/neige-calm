//! The run's step 1, `neige-checkpoint` (#2464 D2): the delivery's own script with the reset line
//! (`GIT_RUN_CHECKPOINT_SCRIPT`), run by the held wrapper after its release, so no checkpoint git
//! runs in the operation drive. It makes the worker's checkout the attempt's one commit above the
//! lease base and pins it under the run's ref; the declared steps then see a clean checkout at it.
//!
//! Also the two bounded reads around a run: the ref the checkpoint pinned, and the digest of the
//! refs a gate names as a base (`refs/remotes`, `refs/tags`), taken when the run is spawned.

use std::path::Path;

use calm_types::forge_git::{
    FORGE_SHELL_PRELUDE, GIT_LEASE_PROVENANCE_SCRIPT, GIT_RUN_CHECKPOINT_SCRIPT,
};
use sha2::{Digest, Sha256};

use super::FrozenRun;
use crate::operation::forge_action_adapter::forge_base_env;
use crate::operation::gate_lifecycle::GateStep;
use crate::operation::gate_process::sh_single_quote;
use crate::operation::task_verify_adapter::SAMPLE_TIMEOUT;
use crate::plugin_host::child_process::run_bounded;

/// The name of the run's first step in its log, step file and answers.
pub(crate) const CHECKPOINT_STEP: &str = "neige-checkpoint";

/// The format of the refs digest: `%(symref)` makes a retargeted `origin/HEAD` visible (K22).
const REFS_FORMAT: &str = "--format=%(objectname) %(refname) %(symref)";

const READ_CAP: usize = 4 * 1024 * 1024;

/// The wrapper's steps: the checkpoint, then the declared ones in order.
pub(crate) fn run_steps(frozen: &FrozenRun) -> Vec<GateStep> {
    let mut steps = Vec::with_capacity(frozen.gate.steps.len() + 1);
    steps.push(GateStep {
        name: CHECKPOINT_STEP.into(),
        cmd: checkpoint_cmd(frozen),
    });
    steps.extend(frozen.gate.steps.iter().cloned());
    steps
}

/// `sh -c '<prelude, provenance, checkpoint>' sh <the delivery's six positional parameters>`, the
/// delivery's argv (`delivery_argv`) as one shell command line.
fn checkpoint_cmd(frozen: &FrozenRun) -> String {
    let script = format!(
        "{FORGE_SHELL_PRELUDE}\n{GIT_LEASE_PROVENANCE_SCRIPT}\n{GIT_RUN_CHECKPOINT_SCRIPT}"
    );
    [
        "sh",
        "-c",
        &script,
        "sh",
        &frozen.message,
        &frozen.branch,
        &frozen.ref_name,
        &frozen.base_sha,
        &frozen.canonical_path,
        &frozen.git_common_dir,
    ]
    .iter()
    .map(|arg| sh_single_quote(arg))
    .collect::<Vec<_>>()
    .join(" ")
}

/// One bounded git read in `cwd` under the sampling bound, in the forge base environment.
async fn git_read(cwd: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(args).current_dir(cwd);
    forge_base_env(&mut cmd);
    run_bounded(cmd, tokio::time::Instant::now() + SAMPLE_TIMEOUT, READ_CAP)
        .await
        .map_err(|error| format!("git {} in {}: {error:?}", args.join(" "), cwd.display()))
}

/// SHA-256 of the remote-tracking refs and tags with their symref targets; `None` when it cannot
/// be taken (then nothing may stand on it).
pub(crate) async fn refs_digest(cwd: &Path) -> Option<String> {
    let output = git_read(
        cwd,
        &["for-each-ref", REFS_FORMAT, "refs/remotes", "refs/tags"],
    )
    .await
    .ok()
    .filter(|output| output.status.success())?;
    Some(
        Sha256::digest(&output.stdout)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    )
}

/// The commit the run's ref names: `Ok(None)` when the ref does not exist (the checkpoint did not
/// finish), `Err` when it cannot be read.
pub(crate) async fn read_run_ref(cwd: &Path, ref_name: &str) -> Result<Option<String>, String> {
    let output = git_read(
        cwd,
        &[
            "rev-parse",
            "--verify",
            "-q",
            &format!("{ref_name}^{{commit}}"),
        ],
    )
    .await?;
    match output.status.code() {
        Some(0) => {
            let commit = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if commit.is_empty() || commit.contains(char::is_whitespace) {
                return Err(format!(
                    "git rev-parse printed no single commit id: {commit:?}"
                ));
            }
            Ok(Some(commit))
        }
        Some(1) => Ok(None),
        _ => Err(format!(
            "git rev-parse {ref_name} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )),
    }
}
