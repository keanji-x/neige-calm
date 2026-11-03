//! Observation facts over original ledger addresses, with public owner acceptance.
use super::{FactCoverageError, classify::*};
use calm_types::{event::*, observation::*, verify_target::*};
use serde_json::Value;

pub(super) fn observation(
    f: &mut FrameView<'_>,
    path: &str,
    system: bool,
) -> Result<(), FactCoverageError> {
    if system {
        f.diagnose::<calm_types::conversation::SystemObservation>(path)?;
    } else {
        f.diagnose::<Observation>(path)?;
    }
    if !f
        .value_at(path)
        .is_some_and(|v| v.is_object() || v.is_array())
    {
        return Ok(());
    }
    let tag_path = if f.value_at(path).is_some_and(Value::is_array) {
        format!("{path}/0")
    } else {
        format!("{path}/type")
    };
    f.check::<String>(&tag_path)?;
    let tag = f.value_at(&tag_path).and_then(Value::as_str);
    let (declaration, optional) = match tag {
        Some("track_goal" | "system_context" | "user_message") => ("type text", ""),
        Some("report_edited") => (
            "type presentation track_id body_sha256 body author body_before doc_rev_after blocks_after",
            "presentation author body_before doc_rev_after blocks_after",
        ),
        Some("task_completed") => (
            "type idempotency_key result result_contract",
            "result_contract",
        ),
        Some("task_failed") => ("type idempotency_key error", ""),
        Some("worker_hook_stop") => (
            "type track_id card_id kind idempotency_key",
            "idempotency_key",
        ),
        Some("task_gate_result") => (
            "type idempotency_key key passed failing_step exit_code log_tail attempt status_detail target",
            "failing_step exit_code status_detail target",
        ),
        Some("task_git_delivery_settled") => {
            ("type key attempt_id result retained_path", "retained_path")
        }
        Some("track_wake") => ("type source key text", ""),
        Some("workspace_leased") => ("type track_id card_id lease_id path", ""),
        Some("workspace_released") => ("type track_id card_id lease_id", ""),
        Some("forge_pr_merged" | "forge_pr_opened") => ("type track_id pr_number", ""),
        Some("forge_scan_completed") => ("type track_id overlapping_prs", ""),
        Some("forge_pr_checks") => (
            "type track_id pr_number conclusion snapshot failed_checks",
            "snapshot failed_checks",
        ),
        Some("forge_issue_closed") => ("type track_id issue_number", ""),
        Some("worktree_provisioned") => ("type track_id card_id path", ""),
        Some("worktree_committed") => ("type track_id card_id commit_sha branch", ""),
        Some("ask_answered") => ("type track_id answers", ""),
        _ => {
            f.block(&tag_path, FactReason::MissingOwnerDecoder)?;
            return Ok(());
        }
    };
    for (name, p) in f.structure(path, declaration, optional)? {
        match name {
            "result" if tag == Some("task_completed") => f.check::<Value>(&p)?,
            "result" => {
                f.check::<calm_types::git_candidate::DeliverySettlement>(&p)?;
                delivery(f, &p)?;
            }
            "presentation" => {
                f.check::<ReportEditPresentation>(&p)?;
                let knowledge = discriminator(f, &p, "version") == Some("knowledge");
                for (_, leaf) in tagged(
                    f,
                    &p,
                    "version",
                    if knowledge { "knowledge_text" } else { "" },
                )? {
                    f.check::<String>(&leaf)?;
                }
                if discriminator(f, &p, "version")
                    .is_some_and(|tag| !matches!(tag, "legacy" | "knowledge"))
                {
                    f.block(&discriminator_path(f, &p, "version"), FactReason::Malformed)?;
                }
            }
            "author" => f.check::<EditAuthor>(&p)?,
            "doc_rev_after" | "pr_number" | "issue_number" => f.check::<u64>(&p)?,
            "attempt" => f.check::<i64>(&p)?,
            "exit_code" => f.check::<i32>(&p)?,
            "passed" => f.check::<bool>(&p)?,
            "kind" => f.check::<HookKind>(&p)?,
            "target" => {
                f.check::<VerifyTarget>(&p)?;
                verify_target(f, &p)?;
            }
            "snapshot" => {
                f.check::<ForgeChecksSnapshot>(&p)?;
                for (name, leaf) in f.structure(
                    &p,
                    "head_sha mergeable all_checks_completed",
                    "all_checks_completed",
                )? {
                    if name == "all_checks_completed" {
                        f.check::<bool>(&leaf)?;
                    } else {
                        f.check::<String>(&leaf)?;
                    }
                }
            }
            "failed_checks" => {
                f.check::<Vec<ForgeFailedCheck>>(&p)?;
                for slot in f.slots(&p) {
                    failed_check(f, &slot)?;
                }
            }
            "blocks_after" => {
                f.check::<Vec<calm_types::report_edit_diff::ReportBlockRef>>(&p)?;
                for slot in f.slots(&p) {
                    f.check::<calm_types::report_edit_diff::ReportBlockRef>(&slot)?;
                    for (name, leaf) in f.structure(&slot, "id rev", "")? {
                        if name == "rev" {
                            f.check::<u32>(&leaf)?;
                        } else {
                            f.check::<String>(&leaf)?;
                        }
                    }
                }
            }
            "answers" => {
                f.check::<Vec<AnsweredQuestion>>(&p)?;
                for slot in f.slots(&p) {
                    f.check::<AnsweredQuestion>(&slot)?;
                    for (_, leaf) in f.structure(&slot, "title answer", "")? {
                        f.check::<String>(&leaf)?;
                    }
                }
            }
            "overlapping_prs" => {
                f.check::<Vec<u64>>(&p)?;
                for slot in f.slots(&p) {
                    f.check::<u64>(&slot)?;
                }
            }
            _ => f.check::<String>(&p)?,
        }
    }
    retire_explained(f);
    Ok(())
}
pub(super) fn retire_explained(f: &mut FrameView<'_>) {
    // Retire only exact sources with a successful owner fact. Unexplained,
    // unknown and opaque descendants retain their original Deferred marker.
    let explained: Vec<_> = f
        .worker
        .fields
        .iter()
        .filter(|fact| {
            matches!(fact.interpretation, Interpretation::Stored(_))
                && same_cell(&fact.source, &f.cell)
                && !f.worker.fields.iter().any(|other| other.source == fact.source
                    && matches!(&other.interpretation, Interpretation::Blocked(reason)
                        if !matches!(reason, FactReason::Unclassified | FactReason::DeferredObservationInterpretation)))
        })
        .map(|fact| fact.source.clone())
        .collect();
    f.worker.fields.retain(|fact| {
        fact.interpretation
            != Interpretation::Blocked(FactReason::DeferredObservationInterpretation)
            || !explained.contains(&fact.source)
    });
}

fn discriminator_path(f: &FrameView<'_>, path: &str, name: &str) -> String {
    format!(
        "{path}/{}",
        if f.value_at(path).is_some_and(Value::is_array) {
            "0"
        } else {
            name
        }
    )
}
fn discriminator<'a>(f: &FrameView<'a>, path: &str, name: &str) -> Option<&'a str> {
    f.value_at(&discriminator_path(f, path, name))?.as_str()
}
// Public tagged owners accept tag-first sequences as well as objects.
// Capture their declaration coordinates through the shared structure traversal.
fn tagged(
    f: &mut FrameView<'_>,
    path: &str,
    tag: &str,
    fields: &str,
) -> Result<Vec<(String, String)>, FactCoverageError> {
    let declaration = format!("{tag} {fields}");
    let mut rest = vec![];
    for (name, leaf) in f.structure(path, &declaration, "")? {
        if name == tag {
            f.check::<String>(&leaf)?;
        } else {
            rest.push((name.into(), leaf));
        }
    }
    Ok(rest)
}
fn unknown_variant(
    f: &mut FrameView<'_>,
    p: &str,
    tag: &str,
) -> Result<&'static str, FactCoverageError> {
    if discriminator(f, p, tag).is_some() {
        f.block(&discriminator_path(f, p, tag), FactReason::Malformed)?;
    }
    Ok("")
}
fn delivery(f: &mut FrameView<'_>, p: &str) -> Result<(), FactCoverageError> {
    let fields = match discriminator(f, p, "kind") {
        Some("candidate") => "candidate_id commit_sha base_sha base_is_ancestor",
        Some("failed") => "code reason retry_allowed",
        _ => unknown_variant(f, p, "kind")?,
    };
    for (name, leaf) in tagged(f, p, "kind", fields)? {
        match name.as_str() {
            "code" => f.check::<calm_types::git_candidate::DeliveryFailureCode>(&leaf)?,
            "base_is_ancestor" | "retry_allowed" => f.check::<bool>(&leaf)?,
            _ => f.check::<String>(&leaf)?,
        }
    }
    Ok(())
}
fn verify_target(f: &mut FrameView<'_>, p: &str) -> Result<(), FactCoverageError> {
    let tag = discriminator(f, p, "kind");
    let fields = match tag {
        Some("candidate") => "candidate_id commit_sha lease_id evidence",
        Some("no_candidate" | "unbound") => "reason",
        _ => unknown_variant(f, p, "kind")?,
    };
    for (name, leaf) in tagged(f, p, "kind", fields)? {
        match name.as_str() {
            "evidence" => {
                f.check::<VerifyTargetEvidence>(&leaf)?;
                verify_evidence(f, &leaf)?;
            }
            "reason" if tag == Some("unbound") => f.check::<UnboundReason>(&leaf)?,
            "reason" => {
                f.check::<NoCandidateReason>(&leaf)?;
                let fields = match discriminator(f, &leaf, "kind") {
                    Some("delivery_pending" | "delivery_failed") => "delivery_id",
                    Some("no_delivery_row") => "",
                    _ => unknown_variant(f, &leaf, "kind")?,
                };
                for (_, p) in tagged(f, &leaf, "kind", fields)? {
                    f.check::<String>(&p)?;
                }
            }
            _ => f.check::<String>(&leaf)?,
        }
    }
    Ok(())
}
fn verify_evidence(f: &mut FrameView<'_>, p: &str) -> Result<(), FactCoverageError> {
    let fields = match discriminator(f, p, "kind") {
        Some("refused") => "cwd before reasons",
        Some("verified") => "cwd before after reasons",
        Some("unsampled") => "phase",
        Some("reused") => "run cwd before",
        _ => unknown_variant(f, p, "kind")?,
    };
    for (name, leaf) in tagged(f, p, "kind", fields)? {
        match name.as_str() {
            "before" | "after" => {
                f.check::<Sample>(&leaf)?;
                for (name, p) in f.structure(&leaf, "head dirty provenance", "")? {
                    match name {
                        "dirty" => strings(f, &p)?,
                        "provenance" => {
                            f.check::<ProvenanceSample>(&p)?;
                            for (name, p) in
                                f.structure(&p, "realpath common_dir registered", "")?
                            {
                                if name == "registered" {
                                    f.check::<bool>(&p)?;
                                } else {
                                    f.check::<String>(&p)?;
                                }
                            }
                        }
                        _ => f.check::<String>(&p)?,
                    }
                }
            }
            "reasons" => {
                f.check::<Vec<MismatchReason>>(&leaf)?;
                for slot in f.slots(&leaf) {
                    f.check::<MismatchReason>(&slot)?;
                }
            }
            "phase" => {
                f.check::<SamplePhase>(&leaf)?;
                let fields = match discriminator(f, &leaf, "kind") {
                    Some("prepare") => "reason",
                    Some("finalize" | "compensation") => "cwd reason",
                    Some("reconciliation") => "cwd last_error",
                    _ => unknown_variant(f, &leaf, "kind")?,
                };
                for (_, p) in tagged(f, &leaf, "kind", fields)? {
                    f.check::<String>(&p)?;
                }
            }
            _ => f.check::<String>(&leaf)?,
        }
    }
    Ok(())
}
fn strings(f: &mut FrameView<'_>, p: &str) -> Result<(), FactCoverageError> {
    f.check::<Vec<String>>(p)?;
    for slot in f.slots(p) {
        f.check::<String>(&slot)?;
    }
    Ok(())
}
fn failed_check(f: &mut FrameView<'_>, p: &str) -> Result<(), FactCoverageError> {
    f.check::<ForgeFailedCheck>(p)?;
    let Some(v) = f.value_at(p).filter(|v| v.is_object()) else {
        return Ok(());
    };
    // Use the actual untagged owner to select its accepted alternative. A bad
    // url beside a valid id must not invent a required url or reject the id.
    let url = matches!(
        serde_json::from_value::<ForgeCheckLocator>(v.clone()),
        Ok(ForgeCheckLocator::Url { .. })
    );
    let optional = if url {
        "id diagnostics"
    } else {
        "url diagnostics"
    };
    for (name, leaf) in f.structure(p, "name url id diagnostics", optional)? {
        if name != "diagnostics" {
            if name == "name" || (name == "url") == url || f.value_at(&leaf).is_some() {
                f.check::<String>(&leaf)?;
            }
            continue;
        }
        f.check::<ForgeCheckDiagnostics>(&leaf)?;
        let fields = match discriminator(f, &leaf, "status") {
            Some("available") => "failed_tests failed_steps error_summary log_url truncated",
            Some("unavailable") => "reason",
            _ => unknown_variant(f, &leaf, "status")?,
        };
        for (name, p) in tagged(f, &leaf, "status", fields)? {
            match name.as_str() {
                "failed_tests" | "failed_steps" => strings(f, &p)?,
                "truncated" => f.check::<bool>(&p)?,
                _ => f.check::<String>(&p)?,
            }
        }
    }
    Ok(())
}

// V2–V4 Checkpoint and V5 ConversationInputCheckpoint declare pending second.
// Interpret the original System payload; released.rs owns whole-frame adaptation.
pub(super) fn released(f: &mut FrameView<'_>) -> Result<(), FactCoverageError> {
    let root = "/canonical_inputs";
    let pending = if f.value_at(root).is_some_and(Value::is_array) {
        format!("{root}/1")
    } else {
        format!("{root}/pending")
    };
    for slot in f.slots(&pending) {
        if f.value_at(&slot).is_some_and(Value::is_object)
            && discriminator(f, &slot, "kind") == Some("system")
        {
            observation(f, &format!("{slot}/observation"), true)?;
        } else if f.value_at(&slot).is_some_and(Value::is_array)
            && f.value_at(&format!("{slot}/0")).and_then(Value::as_str) == Some("system")
        {
            observation(f, &format!("{slot}/2"), true)?;
        }
    }
    Ok(())
}
