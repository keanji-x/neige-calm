//! `calm.plan.list.candidate.carry`: an attempt whose lease started from a kernel carry commit
//! names the replacement receipt, the carry commit `C'` (its `base_sha`) and `C'^1`, the upstream
//! the candidate was merged onto. The parent is read with local `git` after the read transaction
//! commits, on a blocking thread, as `candidate.upstream` is.

use std::path::PathBuf;

use serde_json::{Value, json};

use super::receipt;
use crate::error::Result;
use crate::model::Task;
use crate::operation::Tx;
use crate::operation::workspace_lease::base::BaseSource;
use crate::operation::workspace_lease::facts::{LeaseStates, latest_workspace_lease_for_card_tx};
use crate::workspace_materialize::neige_git_command;

/// The in-transaction half of `candidate.carry`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CarryView {
    receipt_id: String,
    carry_sha: String,
    git_common_dir: PathBuf,
}

/// The carry of `task`'s current attempt, when its latest lease started from a carry commit.
pub(crate) async fn carry_view_tx(tx: &mut Tx<'_>, task: &Task) -> Result<Option<CarryView>> {
    let Some(card_id) = task.worker_card_id.as_deref() else {
        return Ok(None);
    };
    let Some(base) = latest_workspace_lease_for_card_tx(tx, card_id, LeaseStates::Any)
        .await?
        .and_then(|lease| lease.base)
        .filter(|base| base.base_source == BaseSource::Attempt)
    else {
        return Ok(None);
    };
    let Some(receipt) = receipt::by_successor_tx(tx, &task.track_id, &task.key).await? else {
        return Ok(None);
    };
    Ok(Some(CarryView {
        receipt_id: receipt.receipt_id,
        carry_sha: base.base_sha,
        git_common_dir: base.git_common_dir,
    }))
}

/// `{receipt_id, carry_sha, onto_sha}`; `onto_sha` is left out when git cannot read the
/// parent (advisory, like `candidate.upstream`: a read never fails over it).
fn carry_json(view: &CarryView) -> Value {
    let mut command = neige_git_command();
    command
        .arg(format!("--git-dir={}", view.git_common_dir.display()))
        .args(["rev-parse", "--verify", "-q"])
        .arg(format!("{}^1", view.carry_sha));
    let parent = match command.output() {
        Ok(output) if output.status.success() => {
            Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
        }
        Ok(_) | Err(_) => {
            tracing::warn!(carry_sha = %view.carry_sha, "candidate carry: parent unreadable");
            None
        }
    };
    let mut carry = json!({ "receipt_id": view.receipt_id, "carry_sha": view.carry_sha });
    if let Some(parent) = parent.filter(|parent| !parent.is_empty()) {
        carry["onto_sha"] = json!(parent);
    }
    carry
}

/// [`carry_json`] for each entry on a blocking thread; a failed task reads as no carry at all.
pub(crate) async fn carry_json_blocking(views: Vec<Option<CarryView>>) -> Vec<Option<Value>> {
    let len = views.len();
    if views.iter().all(Option::is_none) {
        return vec![None; len];
    }
    tokio::task::spawn_blocking(move || {
        views
            .iter()
            .map(|view| view.as_ref().map(carry_json))
            .collect()
    })
    .await
    .unwrap_or_else(|error| {
        tracing::warn!(%error, "candidate carry task failed");
        vec![None; len]
    })
}
