//! The receipts an input can end with, built before the reservation so the
//! cached unknown receipt already carries every fact of the request; and the
//! two structured refusals (stale observation, control unavailable). Pure
//! JSON constructors; the fences and the write live in `operations.rs`.
use super::input_control::ClaimStep;
use super::replace_plan::ReplacePlan;
use super::screen_diff::ScreenDiff;
use serde_json::{Value, json};
use uuid::Uuid;

/// The three receipts a write can end with, built before the reservation so
/// the cached unknown receipt already carries every fact of the request.
pub(super) struct WriteReceipts {
    pub(super) unknown: Value,
    pub(super) written: Value,
    pub(super) refused: Value,
}
impl WriteReceipts {
    /// `release` stamps `release: {status: "requested"}` on every receipt,
    /// so the cached unknown receipt already carries the release fact; the
    /// release step later updates it to released/not_held/unconfirmed. A
    /// `replace` plan (#1677) is stamped the same way, so a replay returns
    /// the plan the write was derived from and never recomputes it.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        terminal: &str,
        request_key: &str,
        observation: Uuid,
        drift: Option<&Value>,
        steps: Option<usize>,
        replace: Option<&ReplacePlan>,
        release: bool,
    ) -> Self {
        let mut receipts = Self {
            unknown: unknown_receipt(terminal, request_key, observation, drift),
            written: acknowledged_receipt(terminal, request_key, observation, drift, true),
            refused: acknowledged_receipt(terminal, request_key, observation, drift, false),
        };
        if let Some(steps) = steps {
            receipts.each(|receipt| receipt["steps"] = json!(steps));
        }
        if let Some(plan) = replace {
            receipts.each(|receipt| receipt["replace"] = plan.to_json());
        }
        if release {
            receipts.each(|receipt| receipt["release"] = json!({"status":"requested"}));
        }
        receipts
    }
    pub(super) fn attach(&mut self, claim: Option<&ClaimStep>) {
        self.each(|receipt| attach_claim(receipt, claim));
    }
    fn each(&mut self, mut apply: impl FnMut(&mut Value)) {
        apply(&mut self.unknown);
        apply(&mut self.written);
        apply(&mut self.refused);
    }
}
/// `claim` (and, once granted, `control_id`) on every result of a
/// `claim:true` request, so the caller knows what it holds even when the
/// write did not happen.
pub(super) fn attach_claim(receipt: &mut Value, claim: Option<&ClaimStep>) {
    let Some(claim) = claim else {
        return;
    };
    receipt["claim"] = claim.to_json();
    if let ClaimStep::Claimed(control) = claim {
        receipt["control_id"] = json!(control);
    }
}
pub(super) fn merge(target: &mut Value, fields: Value) {
    if let (Some(target), Some(fields)) = (target.as_object_mut(), fields.as_object()) {
        for (key, value) in fields {
            target.insert(key.clone(), value.clone());
        }
    }
}
/// The stale-observation result: the request was not written, and the caller
/// is told what to compare and how to resend. `screen_diff` (#1666 S4) says
/// whether `allow_output_below_cursor` would admit the resend.
pub(super) fn stale_receipt(
    terminal: &str,
    request_key: &str,
    observation: Uuid,
    observed_revision: u64,
    current_revision: u64,
    diff: &ScreenDiff,
) -> Value {
    json!({"terminal_id":terminal,"request_id":request_key,"outcome":"stale_observation",
        "application_result":"unverified","observation_id_used":observation,
        "observed_revision":observed_revision,"current_revision":current_revision,
        "screen_diff":diff.to_json(observed_revision, current_revision),
        "next":"inspect observation.state and screen_diff (that fresh observation is now the latest on this connection); if only rows below the cursor changed (cursor unmoved, rows_changed_at_or_above_cursor 0) and the action edits the draft, resend the same request_id with observation_id omitted and allow_output_below_cursor=true; if only status text changed elsewhere, resend the same request_id with observation_id omitted and allow_output_since_observation=true; else act on the new state. Neither flag bypasses the control, surface, viewport or pending fences"})
}
/// The control-unavailable result (#1666 S3): `claim:true` could not put
/// control in this connection's hands, nothing was written or cached.
pub(super) fn control_unavailable_receipt(
    terminal: &str,
    request_key: &str,
    observation: Uuid,
    status: &str,
    reason: &str,
) -> Value {
    json!({"terminal_id":terminal,"request_id":request_key,"outcome":"control_unavailable",
        "application_result":"unverified","observation_id_used":observation,
        "reason":reason,"claim":{"status":status,"reason":reason},
        "next":"nothing was written; read observation.state role/control_id; when free, observe and resend with claim=true"})
}
/// Every input receipt, whatever its outcome, carries
/// `application_result:"unverified"`: an acknowledgement says bytes reached the
/// PTY, an unknown outcome says not even that is known, and neither says what
/// the application did with them.
fn unknown_receipt(
    terminal: &str,
    request_key: &str,
    observation: Uuid,
    drift: Option<&Value>,
) -> Value {
    let mut receipt = json!({"terminal_id":terminal,"request_id":request_key,"outcome":"unknown","repeat_input":false,
        "application_result":"unverified","observation_id_used":observation,"output_since_observation":drift.is_some()});
    if let Some(drift) = drift {
        receipt["observation_drift"] = drift.clone();
    }
    receipt
}
fn acknowledged_receipt(
    terminal: &str,
    request_key: &str,
    observation: Uuid,
    drift: Option<&Value>,
    written: bool,
) -> Value {
    let mut receipt = json!({"terminal_id":terminal,"request_id":request_key,"outcome":if written{"written"}else{"refused"},
        "application_result":"unverified","next":"observe the application result",
        "observation_id_used":observation,"output_since_observation":drift.is_some()});
    if let Some(drift) = drift {
        receipt["observation_drift"] = drift.clone();
    }
    receipt
}

#[cfg(test)]
mod tests {
    use super::super::screen_diff::{CursorSnapshot, Tolerance};
    use super::*;

    fn diff() -> ScreenDiff {
        let at = CursorSnapshot {
            row: 1,
            column: 0,
            visible: true,
        };
        ScreenDiff::compare(at, &[1, 2, 3], at, &[1, 2, 9])
    }

    /// The field contract is uniform: written, refused and unknown receipts
    /// all say `application_result:"unverified"`; only acknowledged ones add
    /// `next`, and drift evidence is copied whenever it exists.
    #[test]
    fn every_terminal_input_receipt_outcome_reports_application_result_unverified() {
        let observation = Uuid::new_v4();
        let drift = json!({"observed_revision":3,"input_revision":5});
        let unknown = unknown_receipt("t1", "r1", observation, None);
        let written = acknowledged_receipt("t1", "r1", observation, None, true);
        let refused = acknowledged_receipt("t1", "r1", observation, Some(&drift), false);
        for (receipt, outcome) in [
            (&unknown, "unknown"),
            (&written, "written"),
            (&refused, "refused"),
        ] {
            assert_eq!(receipt["outcome"], outcome, "{receipt}");
            assert_eq!(receipt["application_result"], "unverified", "{receipt}");
            assert_eq!(receipt["terminal_id"], "t1");
            assert_eq!(receipt["request_id"], "r1");
            assert_eq!(receipt["observation_id_used"], json!(observation));
            assert!(receipt.get("application_completed").is_none());
        }
        assert_eq!(unknown["repeat_input"], false);
        assert!(unknown.get("next").is_none());
        assert_eq!(unknown["output_since_observation"], false);
        assert_eq!(written["next"], "observe the application result");
        assert_eq!(refused["output_since_observation"], true);
        assert_eq!(refused["observation_drift"], drift);
        assert_eq!(
            unknown_receipt("t1", "r1", observation, Some(&drift))["observation_drift"],
            drift
        );
        let stale = stale_receipt("t1", "r1", observation, 3, 5, &diff());
        assert_eq!(stale["outcome"], "stale_observation");
        assert_eq!(stale["application_result"], "unverified");
        assert_eq!(stale["terminal_id"], "t1");
        assert_eq!(stale["request_id"], "r1");
        assert_eq!(stale["observation_id_used"], json!(observation));
        assert_eq!(stale["observed_revision"], 3);
        assert_eq!(stale["current_revision"], 5);
        let next = stale["next"].as_str().unwrap();
        assert!(next.contains(
            "resend the same request_id with observation_id omitted and allow_output_since_observation=true"
        ));
        assert!(next.contains(
            "resend the same request_id with observation_id omitted and allow_output_below_cursor=true"
        ));
        assert!(
            next.contains("Neither flag bypasses the control, surface, viewport or pending fences")
        );
        assert_eq!(
            stale["screen_diff"],
            json!({"compared":{"observed_revision":3,"current_revision":5},"cursor":{"moved":false,"visible":true},
                "rows_changed_total":1,"rows_changed_at_or_above_cursor":0,"rows_changed_below_cursor":1})
        );
        assert!(stale.get("output_since_observation").is_none());
        assert!(stale.get("observation_drift").is_none());
        let unavailable =
            control_unavailable_receipt("t1", "r1", observation, "unconfirmed", "why");
        assert_eq!(unavailable["outcome"], "control_unavailable");
        assert_eq!(unavailable["application_result"], "unverified");
        assert_eq!(unavailable["reason"], "why");
        assert_eq!(
            unavailable["claim"],
            json!({"status":"unconfirmed","reason":"why"})
        );
        assert!(
            unavailable["next"]
                .as_str()
                .unwrap()
                .contains("nothing was written")
        );
    }

    /// #1666: `steps` on every write receipt of a sequence, `claim` and
    /// `control_id` on every result of a claim, whatever the outcome, and
    /// (r1) `release: requested` on every write receipt of a release request
    /// before the release runs.
    #[test]
    fn write_receipts_carry_steps_claim_and_release_uniformly() {
        let observation = Uuid::new_v4();
        let control = Uuid::new_v4();
        let mut receipts = WriteReceipts::new("t1", "r1", observation, None, Some(4), None, true);
        receipts.attach(Some(&ClaimStep::Claimed(control)));
        for receipt in [&receipts.unknown, &receipts.written, &receipts.refused] {
            assert_eq!(receipt["steps"], 4, "{receipt}");
            assert_eq!(
                receipt["claim"],
                json!({"status":"claimed","control_id":control})
            );
            assert_eq!(receipt["control_id"], json!(control));
            assert_eq!(
                receipt["release"],
                json!({"status":"requested"}),
                "{receipt}"
            );
        }
        let mut plain = WriteReceipts::new("t1", "r1", observation, None, None, None, false);
        plain.attach(Some(&ClaimStep::Held));
        assert!(plain.written.get("steps").is_none());
        assert!(plain.written.get("replace").is_none());
        assert!(plain.written.get("release").is_none());
        assert!(plain.unknown.get("release").is_none());
        assert_eq!(plain.written["claim"], json!({"status":"held"}));
        assert!(plain.written.get("control_id").is_none());
        let mut none = WriteReceipts::new("t1", "r1", observation, None, None, None, false);
        none.attach(None);
        assert!(none.written.get("claim").is_none());
        // #1677: the derived plan on every write receipt, unknown included,
        // so the cached receipt carries it before the write is sent.
        let plan = ReplacePlan {
            row: 3,
            cursor_index: 12,
            cursor_visible: false,
            moves: Some(super::super::replace_plan::Moves {
                key: "Left",
                repeat: 5,
            }),
            erased: 2,
            inserted: "19".into(),
        };
        let replaced = WriteReceipts::new("t1", "r1", observation, None, None, Some(&plan), false);
        for receipt in [&replaced.unknown, &replaced.written, &replaced.refused] {
            assert_eq!(
                receipt["replace"],
                json!({"row":3,"cursor_index":12,"cursor_visible":false,"moves":{"key":"Left","repeat":5},"erased":2,"inserted":"19"}),
                "{receipt}"
            );
            assert!(receipt.get("steps").is_none());
        }
        let mut stale = stale_receipt("t1", "r1", observation, 3, 5, &diff());
        attach_claim(&mut stale, Some(&ClaimStep::Claimed(control)));
        assert_eq!(stale["claim"]["status"], "claimed");
        assert_eq!(stale["control_id"], json!(control));
        let mut drift = json!({"observed_revision":3,"input_revision":5});
        merge(&mut drift, diff().tolerance_json(Tolerance::BelowCursor));
        assert_eq!(
            drift,
            json!({"observed_revision":3,"input_revision":5,"tolerance":"below_cursor",
                "rows_changed_below_cursor":[2],"rows_changed_total":1,"truncated":false})
        );
        // #1683: the wide opt-in merges its own shape onto the same revisions.
        let mut wide = json!({"observed_revision":3,"input_revision":5});
        merge(
            &mut wide,
            diff().tolerance_json(Tolerance::OutputSinceObservation),
        );
        assert_eq!(
            wide,
            json!({"observed_revision":3,"input_revision":5,"tolerance":"output_since_observation",
                "cursor":{"moved":false,"visible":true},"rows_changed_total":1,
                "rows_changed_at_or_above_cursor":0,"rows_changed_below_cursor":1,
                "rows_changed":[2],"truncated":false})
        );
    }
}
