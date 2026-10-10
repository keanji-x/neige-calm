//! The receipts an input can end with, built before the reservation so the cached unknown
//! receipt already carries every fact of the request. Pure JSON constructors.
use super::input_control::ClaimStep;
use super::screen_diff::ScreenDiff;
use super::write_leg::Delivered;
use serde_json::{Value, json};
use uuid::Uuid;

/// The three receipts a write can end with, built before the reservation.
pub(super) struct WriteReceipts {
    pub(super) unknown: Value,
    pub(super) written: Value,
    pub(super) refused: Value,
}
impl WriteReceipts {
    /// `release` is stamped on every receipt up front, so a replay returns the facts the write
    /// was derived from and never recomputes them.
    pub(super) fn new(
        terminal: &str,
        idempotency_key: &str,
        observation: Uuid,
        drift: Option<&Value>,
        steps: Option<usize>,
        release: bool,
    ) -> Self {
        let mut receipts = Self {
            unknown: unknown_receipt(terminal, idempotency_key, observation, drift),
            written: acknowledged_receipt(terminal, idempotency_key, observation, drift, true),
            refused: acknowledged_receipt(terminal, idempotency_key, observation, drift, false),
        };
        if let Some(steps) = steps {
            receipts.each(|receipt| receipt["steps"] = json!(steps));
        }
        if release {
            receipts.each(|receipt| receipt["release"] = json!({"status":"requested"}));
        }
        receipts
    }
    /// The receipt of `outcome`; a refusal carries its reason.
    pub(super) fn for_outcome(&self, outcome: &Delivered) -> Value {
        match outcome {
            Delivered::Written => self.written.clone(),
            Delivered::Unknown => self.unknown.clone(),
            Delivered::Refused(reason) => {
                let mut refused = self.refused.clone();
                refused["reason"] = json!(reason);
                refused
            }
        }
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
/// `claim` on every result of a `claim:true` request, so the caller knows what it holds even
/// when the write did not happen.
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
/// The stale-observation result: the request was not written.
pub(super) fn stale_receipt(
    terminal: &str,
    idempotency_key: &str,
    observation: Uuid,
    observed_revision: u64,
    current_revision: u64,
    diff: &ScreenDiff,
) -> Value {
    json!({"terminal_id":terminal,"idempotency_key":idempotency_key,"outcome":"stale_observation",
        "application_result":"unverified","observation_id_used":observation,
        "observed_revision":observed_revision,"current_revision":current_revision,
        "screen_diff":diff.to_json(observed_revision, current_revision),
        "next":"inspect observation.state and screen_diff (that fresh observation is now the latest on this connection); if only status text changed, resend the same idempotency_key with observation_id omitted and allow_output_since_observation=true; else act on the new state. The flag bypasses none of the control, surface, viewport or pending fences"})
}
/// The control-unavailable result: nothing was written or cached.
pub(super) fn control_unavailable_receipt(
    terminal: &str,
    idempotency_key: &str,
    observation: Uuid,
    status: &str,
    reason: &str,
) -> Value {
    json!({"terminal_id":terminal,"idempotency_key":idempotency_key,"outcome":"control_unavailable",
        "application_result":"unverified","observation_id_used":observation,
        "reason":reason,"claim":{"status":status,"reason":reason},
        "next":"nothing was written; read observation.state role/control_id; when free, read and resend with claim=true"})
}
/// Every input receipt carries `application_result:"unverified"`: an acknowledgement says
/// bytes reached the PTY, not what the application did with them.
fn unknown_receipt(
    terminal: &str,
    idempotency_key: &str,
    observation: Uuid,
    drift: Option<&Value>,
) -> Value {
    let mut receipt = json!({"terminal_id":terminal,"idempotency_key":idempotency_key,"outcome":"unknown","repeat_input":false,
        "application_result":"unverified","observation_id_used":observation,"output_since_observation":drift.is_some()});
    if let Some(drift) = drift {
        receipt["observation_drift"] = drift.clone();
    }
    receipt
}
fn acknowledged_receipt(
    terminal: &str,
    idempotency_key: &str,
    observation: Uuid,
    drift: Option<&Value>,
    written: bool,
) -> Value {
    let next = if written {
        "read the application result"
    } else {
        "nothing was written; read again"
    };
    let mut receipt = json!({"terminal_id":terminal,"idempotency_key":idempotency_key,"outcome":if written{"written"}else{"refused"},
        "application_result":"unverified","next":next,
        "observation_id_used":observation,"output_since_observation":drift.is_some()});
    if let Some(drift) = drift {
        receipt["observation_drift"] = drift.clone();
    }
    receipt
}

#[cfg(test)]
mod tests {
    use super::super::screen_diff::CursorSnapshot;
    use super::*;

    fn diff() -> ScreenDiff {
        let at = CursorSnapshot {
            row: 1,
            column: 0,
            visible: true,
        };
        ScreenDiff::compare(at, &[1, 2, 3], at, &[1, 2, 9])
    }

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
            assert_eq!(receipt["idempotency_key"], "r1");
            assert_eq!(receipt["observation_id_used"], json!(observation));
            assert!(receipt.get("application_completed").is_none());
        }
        assert_eq!(unknown["repeat_input"], false);
        assert!(unknown.get("next").is_none());
        assert_eq!(unknown["output_since_observation"], false);
        assert_eq!(written["next"], "read the application result");
        assert_eq!(refused["output_since_observation"], true);
        assert_eq!(refused["next"], "nothing was written; read again");
        assert_eq!(refused["observation_drift"], drift);
        assert_eq!(
            unknown_receipt("t1", "r1", observation, Some(&drift))["observation_drift"],
            drift
        );
        let stale = stale_receipt("t1", "r1", observation, 3, 5, &diff());
        assert_eq!(stale["outcome"], "stale_observation");
        assert_eq!(stale["application_result"], "unverified");
        assert_eq!(stale["terminal_id"], "t1");
        assert_eq!(stale["idempotency_key"], "r1");
        assert_eq!(stale["observation_id_used"], json!(observation));
        assert_eq!(stale["observed_revision"], 3);
        assert_eq!(stale["current_revision"], 5);
        let next = stale["next"].as_str().unwrap();
        assert!(next.contains(
            "resend the same idempotency_key with observation_id omitted and allow_output_since_observation=true"
        ));
        assert!(next.contains(
            "The flag bypasses none of the control, surface, viewport or pending fences"
        ));
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

    #[test]
    fn write_receipts_carry_steps_claim_and_release_uniformly() {
        let observation = Uuid::new_v4();
        let control = Uuid::new_v4();
        let mut receipts = WriteReceipts::new("t1", "r1", observation, None, Some(4), true);
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
        assert_eq!(
            receipts.for_outcome(&Delivered::Refused("why".into()))["reason"],
            "why"
        );
        assert_eq!(receipts.for_outcome(&Delivered::Written), receipts.written);
        assert_eq!(receipts.for_outcome(&Delivered::Unknown), receipts.unknown);
        let mut plain = WriteReceipts::new("t1", "r1", observation, None, None, false);
        plain.attach(Some(&ClaimStep::Held));
        assert!(plain.written.get("steps").is_none());
        assert!(plain.written.get("release").is_none());
        assert!(plain.unknown.get("release").is_none());
        assert_eq!(plain.written["claim"], json!({"status":"held"}));
        assert!(plain.written.get("control_id").is_none());
        let mut none = WriteReceipts::new("t1", "r1", observation, None, None, false);
        none.attach(None);
        assert!(none.written.get("claim").is_none());
        let mut stale = stale_receipt("t1", "r1", observation, 3, 5, &diff());
        attach_claim(&mut stale, Some(&ClaimStep::Claimed(control)));
        assert_eq!(stale["claim"]["status"], "claimed");
        assert_eq!(stale["control_id"], json!(control));
        // The opt-in merges its shape onto the revisions.
        let mut wide = json!({"observed_revision":3,"input_revision":5});
        merge(&mut wide, diff().drift_json());
        assert_eq!(
            wide,
            json!({"observed_revision":3,"input_revision":5,"tolerance":"output_since_observation",
                "cursor":{"moved":false,"visible":true},"rows_changed_total":1,
                "rows_changed_at_or_above_cursor":0,"rows_changed_below_cursor":1,
                "rows_changed":[2],"truncated":false})
        );
    }
}
