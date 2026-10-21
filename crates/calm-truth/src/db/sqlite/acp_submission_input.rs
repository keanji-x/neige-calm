//! Strict receipt association. Historical bytes retain their original unversioned shape.
use crate::error::{CalmError, Result};
use calm_types::conversation::{InputDeliveryFact, LogicalTurnId, SubmissionDispatchProof};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcpCanonicalInput {
    pub logical_turn_id: LogicalTurnId,
    pub proof: SubmissionDispatchProof,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcpHistoricalInput {
    pub input: Value,
    pub prompt: Value,
    pub queue: Value,
}
#[derive(Clone, Debug, PartialEq)]
pub enum AcpSubmissionInput {
    Historical(AcpHistoricalInput),
    Canonical(AcpCanonicalInput),
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "version", rename_all = "snake_case", deny_unknown_fields)]
enum Versioned {
    CanonicalV1 { association: AcpCanonicalInput },
}
impl AcpSubmissionInput {
    pub fn decode(bytes: &str) -> Result<Self> {
        let value: Value = serde_json::from_str(bytes)?;
        if value.get("version").is_some() {
            match serde_json::from_str(bytes)? {
                Versioned::CanonicalV1 { association } => Ok(Self::Canonical(association)),
            }
        } else {
            Ok(Self::Historical(serde_json::from_str(bytes)?))
        }
    }
    pub fn encode(&self) -> Result<String> {
        Ok(match self {
            Self::Historical(input) => serde_json::to_string(input)?,
            Self::Canonical(association) => serde_json::to_string(&Versioned::CanonicalV1 {
                association: association.clone(),
            })?,
        })
    }
}

/// The receipt writer validates proof and effective claims in its own transaction.
pub(super) async fn validate_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    receipt: &super::AcpSubmission,
    association: &AcpCanonicalInput,
) -> Result<()> {
    use super::conversation::*;
    let attempt = association.proof.attempt();
    if receipt.client_id != attempt.submission_id.as_str() {
        return Err(CalmError::Conflict("ACP receipt names another submission"));
    }
    let identity = submission_identity_tx(tx, &attempt.submission_id).await?;
    let carrier =
        current_input_carrier_tx(tx, &identity.card_id, &receipt.worker_session_id).await?;
    let stored = submission_plan_tx(tx, &attempt.submission_id).await?;
    let proof = SubmissionDispatchProof::from_stored_plan(
        attempt.clone(),
        &stored.plan,
        stored.original_digest,
    )
    .map_err(|_| CalmError::Conflict("ACP original proof changed"))?;
    let claims: i64 = sqlx::query_scalar("SELECT count(*) FROM conversation_submission_inputs WHERE submission_id=?1 AND effective=1")
        .bind(attempt.submission_id.as_str()).fetch_one(&mut **tx).await?;
    if proof != association.proof
        || identity.logical_turn_id != association.logical_turn_id
        || carrier
            .snapshot()
            .get("last_thread_id")
            .and_then(Value::as_str)
            != Some(receipt.thread_id.as_str())
        || carrier.checkpoint().active.as_ref().is_none_or(|active| {
            active.logical_turn_id != association.logical_turn_id
                || !active.submission_ids.contains(&attempt.submission_id)
        })
        || !carrier
            .checkpoint()
            .attempt(&attempt.submission_id)
            .is_some_and(|fact| {
                fact.attempt == *attempt
                    && matches!(fact.delivery, InputDeliveryFact::CrossingUnknown)
            })
        || usize::try_from(claims).ok() != Some(proof.input_refs().len())
    {
        return Err(CalmError::Conflict(
            "ACP canonical receipt no longer owns its crossing",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn acp_input_codec_is_explicit_strict_and_body_free() {
        let old = r#"{"input":[{"type":"text","text":"historical"}],"prompt":[],"queue":[]}"#;
        assert!(matches!(
            AcpSubmissionInput::decode(old).unwrap(),
            AcpSubmissionInput::Historical(_)
        ));
        let proof = serde_json::from_value(json!({
            "version":"v2",
            "attempt":{"submission_id":"11111111111111111111111111111111","attempt_id":"22222222222222222222222222222222"},
            "input_refs":[{"kind":"system","observation_id":"33333333333333333333333333333333"}],
            "render_plan":{"version":"v1","submission_id":"11111111111111111111111111111111","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}
        })).unwrap();
        let input = AcpSubmissionInput::Canonical(AcpCanonicalInput {
            logical_turn_id: LogicalTurnId::parse("44444444444444444444444444444444").unwrap(),
            proof,
        });
        let bytes = input.encode().unwrap();
        assert_eq!(AcpSubmissionInput::decode(&bytes).unwrap(), input);
        let value: Value = serde_json::from_str(&bytes).unwrap();
        assert_eq!(value["version"], "canonical_v1");
        for field in ["input", "prompt", "queue", "invocation"] {
            let mut bad = value.clone();
            bad[field] = json!("body");
            assert!(AcpSubmissionInput::decode(&bad.to_string()).is_err());
            let mut bad = value.clone();
            bad["association"][field] = json!("body");
            assert!(AcpSubmissionInput::decode(&bad.to_string()).is_err());
        }
        let duplicate = format!(
            "{{\"version\":\"canonical_v1\",\"association\":{},\"association\":{}}}",
            value["association"], value["association"]
        );
        assert!(AcpSubmissionInput::decode(&duplicate).is_err());
        for bad in [
            json!({"version":"future","input":[],"prompt":[],"queue":[]}),
            json!({"version":null,"input":[],"prompt":[],"queue":[]}),
            json!({"association":value["association"]}),
            json!({"version":"canonical_v1","association":{}}),
        ] {
            assert!(AcpSubmissionInput::decode(&bad.to_string()).is_err());
        }
    }
}
