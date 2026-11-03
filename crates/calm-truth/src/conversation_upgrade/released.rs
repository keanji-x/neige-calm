//! Released evidence projection. Adaptation belongs exclusively to calm-types.
use super::{FactCoverageError, classify::*};
use calm_types::{conversation::*, harness::snapshot::CanonicalHarnessSnapshot};
use serde::de::DeserializeOwned;
use serde_json::Value;

const ADAPTER: &str = "calm_types::harness::snapshot::CanonicalHarnessSnapshot::from_released";

pub(super) fn facts(f: &mut FrameView<'_>, version: u64) -> Result<(), FactCoverageError> {
    // CanonicalFrameWire flattens the header into an object, never V1's sequence.
    super::nested::released_header(f)?;
    let mut p = Projection { f, aliases: vec![] };
    p.fields("", "schema_version mode canonical_inputs phase turn_budget push_watermark last_thread_id last_turn_id last_report_body_sha256 last_seen_head issued_turn_head wedged_reason interruption_intent token_usage compact_outcome context_usage_invalidated context_usage_invalidated_turn pending_rewind last_turn_base",
        "last_thread_id last_turn_id last_report_body_sha256 last_seen_head issued_turn_head wedged_reason interruption_intent token_usage compact_outcome context_usage_invalidated_turn pending_rewind last_turn_base")?;
    let c = "/canonical_inputs";
    if p.value(c).is_some_and(|v| v.is_object() || v.is_array()) {
        // Private released checkpoints have no public standalone decoder. We record
        // presence against their declarations; only the whole adapter accepts them.
        p.fields(
            c,
            if version >= 4 {
                "next_queue_position pending active attempts receipts"
            } else {
                "next_queue_position pending active attempts"
            },
            "active",
        )?;
        p.leaf::<u64>(&format!("{c}/next_queue_position"))?;
        p.check::<ActiveInputSubmissions>(&format!("{c}/active"), false)?;
        if p.value(&format!("{c}/active"))
            .is_some_and(|v| v.is_object() || v.is_array())
        {
            p.shape::<ActiveInputSubmissions>(
                &format!("{c}/active"),
                "logical_turn_id submission_ids",
                "",
            )?;
            p.leaf::<LogicalTurnId>(&format!("{c}/active/logical_turn_id"))?;
            p.list::<InputSubmissionId>(&format!("{c}/active/submission_ids"))?;
        }
        for slot in p.slots(&format!("{c}/pending")) {
            pending(&mut p, &slot, version)?;
        }
        for slot in p.slots(&format!("{c}/attempts")) {
            attempt(&mut p, &slot, version)?;
        }
        if version >= 4 {
            for slot in p.slots(&format!("{c}/receipts")) {
                receipt(&mut p, &slot, version)?;
            }
        }
        // Container type errors do not stop sibling traversal or the raw inventory.
        for name in ["pending", "attempts"] {
            p.check::<Vec<Value>>(&format!("{c}/{name}"), false)?;
        }
        if version >= 4 {
            p.check::<Vec<Value>>(&format!("{c}/receipts"), false)?;
        }
    } else {
        p.f.block(c, FactReason::Malformed)?;
    }
    if version == 5 {
        p.check::<ConversationInputCheckpoint>(c, false)?;
    }
    match CanonicalHarnessSnapshot::from_released(p.f.value.clone()) {
        Ok(adapted) => {
            let mut derived = vec![];
            p.derived("", &serde_json::to_value(&adapted)?, &mut derived);
            p.f.worker.frame = Some(FrameFacts::DerivedByReleasedAdapter {
                original_version: version,
                adapted: Box::new(adapted),
                derived,
            });
        }
        Err(_) => {
            p.f.block("", FactReason::Malformed)?;
            // The adapter returns prose, not a typed private-checkpoint path.
            // Bind the real checkpoint ancestor instead of guessing a leaf.
            if version < 5 {
                p.f.block(c, FactReason::MissingOwnerDecoder)?;
            }
        }
    }
    super::observation_projection::retire_explained(p.f);
    p.f.block(c, FactReason::RelationshipRequiresP2)
}

// Resolve each segment through shared structure coordinates in the original cell.
// Coordinates never become fabricated ledger SourceRefs.
struct Projection<'f, 'a> {
    f: &'f mut FrameView<'a>,
    // Released V4 relocates request/result beneath the new decision.
    aliases: Vec<(String, String)>,
}
impl Projection<'_, '_> {
    fn path(&self, path: &str) -> String {
        let path = self
            .aliases
            .iter()
            .rev()
            .find_map(|(canonical, original)| {
                path.strip_prefix(canonical)
                    .filter(|tail| tail.is_empty() || tail.starts_with('/'))
                    .map(|tail| format!("{original}{tail}"))
            })
            .unwrap_or_else(|| path.into());
        path.split('/').skip(1).fold(String::new(), |parent, name| {
            let field = format!("{parent}/{name}");
            self.f
                .worker
                .sequence_fields
                .iter()
                .rev()
                .find(|(declared, _)| *declared == field)
                .map_or(field.clone(), |(_, original)| original.clone())
        })
    }

    fn value(&self, path: &str) -> Option<&Value> {
        self.f.value_at(&self.path(path))
    }
    fn fields(
        &mut self,
        path: &str,
        fields: &str,
        optional: &str,
    ) -> Result<(), FactCoverageError> {
        let original = self.path(path);
        self.f.structure(&original, fields, optional)?;
        Ok(())
    }
    fn shape<T: DeserializeOwned>(
        &mut self,
        path: &str,
        fields: &str,
        optional: &str,
    ) -> Result<(), FactCoverageError> {
        self.check::<T>(path, false)?;
        self.fields(path, fields, optional)
    }
    fn check<T: DeserializeOwned>(
        &mut self,
        path: &str,
        store: bool,
    ) -> Result<(), FactCoverageError> {
        let path = self.path(path);
        match self.f.value_at(&path) {
            None | Some(Value::Null) => self.f.check::<T>(&path),
            // from_released uses owned Value. Borrowed Value and text are not
            // assumed to have the same accepted representations.
            Some(v) => self.f.decoded(
                &path,
                serde_path_to_error::deserialize::<_, T>(v.clone()),
                store,
            ),
        }
    }
    fn leaf<T: DeserializeOwned>(&mut self, path: &str) -> Result<(), FactCoverageError> {
        self.check::<T>(path, true)
    }
    fn slots(&self, path: &str) -> Vec<String> {
        (0..self
            .value(path)
            .and_then(Value::as_array)
            .map_or(0, Vec::len))
            .map(|i| format!("{path}/{i}"))
            .collect()
    }
    fn list<T: DeserializeOwned>(&mut self, path: &str) -> Result<(), FactCoverageError> {
        self.check::<Vec<T>>(path, false)?;
        for slot in self.slots(path) {
            self.leaf::<T>(&slot)?;
        }
        Ok(())
    }
    fn tag(&self, path: &str) -> Option<&str> {
        self.value(path)?
            .get("kind")
            .or_else(|| self.value(path)?.as_array()?.first())?
            .as_str()
    }
    fn tagged<T: DeserializeOwned>(
        &mut self,
        path: &str,
        fields: &str,
        optional: &str,
    ) -> Result<(), FactCoverageError> {
        self.check::<T>(path, false)?;
        if self
            .value(path)
            .is_some_and(|v| v.is_object() || v.is_array())
        {
            self.fields(path, &format!("kind {fields}"), optional)?;
            self.leaf::<String>(&format!("{path}/kind"))?;
        }
        Ok(())
    }
    fn derived(&self, path: &str, value: &Value, out: &mut Vec<DerivedValue>) {
        if self.f.value.pointer(path) != Some(value) {
            out.push(DerivedValue {
                ancestor: self.f.nearest_anchor(&self.path(path)),
                target_path: path.into(),
                value: value.clone(),
                owner_adapter: ADAPTER,
            });
        }
        match value {
            Value::Object(m) => {
                for (key, child) in m {
                    self.derived(
                        &format!("{path}/{}", key.replace('~', "~0").replace('/', "~1")),
                        child,
                        out,
                    );
                }
            }
            Value::Array(a) => {
                for (i, child) in a.iter().enumerate() {
                    self.derived(&format!("{path}/{i}"), child, out);
                }
            }
            _ => {}
        }
    }
}
fn pending(p: &mut Projection<'_, '_>, path: &str, version: u64) -> Result<(), FactCoverageError> {
    if !p.value(path).is_some_and(|v| v.is_object() || v.is_array()) {
        return p.check::<PendingInput>(path, false);
    }
    let fields = match p.tag(path) {
        Some("human") => "message queue_position queued_at_ms envelope_id",
        Some("system") => {
            "observation_id observation queue_position queued_at_ms envelope_id addressable"
        }
        Some("explicit_successor") if version == 5 => "retry_intent_id queue_position queued_at_ms",
        _ => "",
    };
    p.tagged::<PendingInput>(path, fields, "envelope_id")?;
    if fields.is_empty() {
        return Ok(());
    }
    for name in fields.split_whitespace() {
        let leaf = format!("{path}/{name}");
        match name {
            "message" => {
                p.shape::<MessageRevisionRef>(&leaf, "message_id revision", "")?;
                p.leaf::<ConversationMessageId>(&format!("{leaf}/message_id"))?;
                p.leaf::<std::num::NonZeroU32>(&format!("{leaf}/revision"))?;
            }
            "observation" => {} // OBS already traverses the exact original payload.
            "observation_id" => p.leaf::<SystemObservationId>(&leaf)?,
            "retry_intent_id" => p.leaf::<RetryIntentId>(&leaf)?,
            "queue_position" => p.leaf::<u64>(&leaf)?,
            "addressable" => p.leaf::<bool>(&leaf)?,
            _ => p.leaf::<i64>(&leaf)?,
        }
    }
    Ok(())
}
fn attempt(p: &mut Projection<'_, '_>, path: &str, version: u64) -> Result<(), FactCoverageError> {
    let fields = if version == 2 {
        "attempt correlation delivery"
    } else {
        "revision invocation attempt correlation delivery"
    };
    if version == 2 {
        p.shape::<InputAttemptFact>(path, fields, "")?;
    } else {
        p.shape::<InputAttemptCheckpoint>(path, fields, "")?;
    }
    if !p.value(path).is_some_and(|v| v.is_object() || v.is_array()) {
        return Ok(());
    }
    let a = format!("{path}/attempt");
    p.shape::<InputAttempt>(&a, "submission_id attempt_id", "")?;
    p.leaf::<InputSubmissionId>(&format!("{a}/submission_id"))?;
    p.leaf::<InputAttemptId>(&format!("{a}/attempt_id"))?;
    let c = format!("{path}/correlation");
    p.shape::<InputCorrelationFact>(
        &c,
        "provider thread_id native_session_id local_turn_id native_turn_id rpc_request_id",
        "native_session_id local_turn_id native_turn_id rpc_request_id",
    )?;
    p.leaf::<calm_types::runtime::AgentProvider>(&format!("{c}/provider"))?;
    for name in [
        "thread_id",
        "native_session_id",
        "local_turn_id",
        "native_turn_id",
    ] {
        p.leaf::<String>(&format!("{c}/{name}"))?;
    }
    p.leaf::<u64>(&format!("{c}/rpc_request_id"))?;
    if version >= 3 {
        p.leaf::<u64>(&format!("{path}/revision"))?;
        let i = format!("{path}/invocation");
        let recorded = p.tag(&i) == Some("recorded");
        p.tagged::<InputInvocation>(&i, if recorded { "configuration" } else { "" }, "")?;
        if recorded {
            let c = format!("{i}/configuration");
            p.shape::<FrozenInputInvocation>(&c, "model effort permission", "")?;
            for name in ["model", "effort"] {
                p.leaf::<Option<String>>(&format!("{c}/{name}"))?;
            }
            p.leaf::<calm_types::harness::PlannerPermissionMode>(&format!("{c}/permission"))?;
        }
    }
    let d = format!("{path}/delivery");
    let fields = match p.tag(&d) {
        Some("accepted") => "basis evidence",
        Some("not_dispatched") => "basis",
        _ => "",
    };
    p.tagged::<InputDeliveryFact>(&d, fields, "")?;
    if fields.contains("basis") {
        p.leaf::<String>(&format!("{d}/basis"))?;
    }
    if fields.contains("evidence") {
        let e = format!("{d}/evidence");
        let fields = match p.tag(&e) {
            Some("native_turn") => "native_turn_id",
            Some("submission") => "submission_id",
            _ => "",
        };
        p.tagged::<NativeInputAcceptance>(&e, fields, "")?;
        for name in fields.split_whitespace() {
            if name == "submission_id" {
                p.leaf::<InputSubmissionId>(&format!("{e}/{name}"))?;
            } else {
                p.leaf::<String>(&format!("{e}/{name}"))?;
            }
        }
    }
    Ok(())
}
fn receipt(p: &mut Projection<'_, '_>, path: &str, version: u64) -> Result<(), FactCoverageError> {
    let fields = if version == 4 {
        "key principal fingerprint request result"
    } else {
        "key principal fingerprint decision"
    };
    p.fields(path, fields, "")?;
    for name in ["key", "principal", "fingerprint"] {
        p.leaf::<String>(&format!("{path}/{name}"))?;
    }
    let decision = if version == 4 {
        path.into()
    } else {
        format!("{path}/decision")
    };
    if version == 5 && !matches!(p.tag(&decision), Some("keep" | "retry")) {
        return p.tagged::<HumanInputResolutionDecision>(&decision, "", "");
    }
    let retry = version == 5 && p.tag(&decision) == Some("retry");
    if version == 5 {
        p.check::<HumanInputResolutionReceipt>(path, false)?;
        p.tagged::<HumanInputResolutionDecision>(
            &decision,
            if retry {
                "predecessor retry_intent_id revision waiting_for_settlement"
            } else {
                "request result"
            },
            "",
        )?;
    }
    let request = format!(
        "{decision}/{}",
        if retry { "predecessor" } else { "request" }
    );
    p.shape::<KeepConversationInputRequest>(
        &request,
        "carrier_id submission_id attempt_id revision",
        "",
    )?;
    for name in ["carrier_id", "submission_id", "attempt_id", "revision"] {
        p.leaf::<String>(&format!("{request}/{name}"))?;
    }
    if retry {
        p.leaf::<RetryIntentId>(&format!("{decision}/retry_intent_id"))?;
        p.leaf::<String>(&format!("{decision}/revision"))?;
        p.leaf::<bool>(&format!("{decision}/waiting_for_settlement"))?;
    } else {
        let r = format!("{decision}/result");
        p.shape::<KeepConversationInputResult>(
            &r,
            "carrier_id submission_id attempt_id revision waiting_for_settlement",
            "",
        )?;
        for name in ["carrier_id", "submission_id", "attempt_id", "revision"] {
            p.leaf::<String>(&format!("{r}/{name}"))?;
        }
        p.leaf::<bool>(&format!("{r}/waiting_for_settlement"))?;
    }
    if version == 4 {
        for name in ["request", "result"] {
            p.aliases.push((
                format!("{path}/decision/{name}"),
                p.path(&format!("{path}/{name}")),
            ));
        }
    }
    Ok(())
}
