//! What every keyed write route shares: the `Idempotency-Key` header, the request fingerprint a key
//! is bound to, how a stored operation's failure is answered, and the answer of a keyed create whose
//! key binds exactly one `operations` row.

use crate::error::{CalmError, Result};
use crate::model::Card;
use crate::operation::{OperationOutcome, PhaseTag};
use crate::session_projection_lookup::project_runtime_into_card_payload;
use crate::session_projection_repo::WorkerSessionProjectionRepo;
use axum::Json;
use axum::http::{HeaderMap, StatusCode};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// The longest `Idempotency-Key` any keyed route stores, in bytes (the key is ASCII). Every client
/// mints far less: the browser a 36-character UUID (`fe/web/src/app/providers/idempotency-key.ts`),
/// the e2e scripts `uuid4`, the kernel's own Today summary `today-summary`. The cap bounds what a
/// binding row keeps for as long as its card or Track lives, with room for a prefixed UUID or ULID.
pub(crate) const IDEMPOTENCY_KEY_MAX_LEN: usize = 128;

/// The one `Idempotency-Key` parser every keyed route shares. A key that is not visible ASCII, is
/// blank, or is longer than [`IDEMPOTENCY_KEY_MAX_LEN`] is 400 `idempotency_key_invalid`.
pub(crate) fn parse_idempotency_key_header(headers: &HeaderMap) -> Result<Option<String>> {
    match headers.get("idempotency-key") {
        Some(value) => {
            let value = value.to_str().map_err(|_| {
                CalmError::IdempotencyKeyInvalid("the header holds non-ASCII bytes".into())
            })?;
            let value = value.trim();
            if value.is_empty() {
                return Err(CalmError::IdempotencyKeyInvalid(
                    "the header is empty".into(),
                ));
            }
            if value.len() > IDEMPOTENCY_KEY_MAX_LEN {
                return Err(CalmError::IdempotencyKeyInvalid(format!(
                    "the key is {} bytes; at most {IDEMPOTENCY_KEY_MAX_LEN} are accepted",
                    value.len()
                )));
            }
            Ok(Some(value.to_string()))
        }
        None => Ok(None),
    }
}

/// A stored operation's failure as the route answers it: a refusal keeps its class; a failure past
/// the commit is `Internal`.
pub(crate) fn calm_error_from_operation_failure(
    last_error_class: Option<&str>,
    last_error: String,
    from_phase: PhaseTag,
) -> CalmError {
    match last_error_class {
        Some("bad_request") => CalmError::BadRequest(last_error),
        Some("not_found") => CalmError::NotFound(last_error),
        Some("forbidden") => CalmError::Forbidden(last_error),
        Some("conflict") => CalmError::Conflict(last_error),
        Some("unauthorized") => CalmError::Unauthorized,
        _ if from_phase == PhaseTag::Pending => CalmError::BadRequest(last_error),
        _ => CalmError::Internal(last_error),
    }
}

// `pub` so the scheduler integration tests can construct idempotency-matched operations.
pub fn stable_payload_hash<T: Serialize>(value: &T) -> Result<String> {
    let value = canonical_json(serde_json::to_value(value)?);
    let bytes = serde_json::to_vec(&value)?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(hex::encode(hasher.finalize()))
}

fn canonical_json(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(canonical_json).collect())
        }
        serde_json::Value::Object(map) => {
            let sorted: BTreeMap<_, _> = map
                .into_iter()
                .map(|(key, value)| (key, canonical_json(value)))
                .collect();
            serde_json::Value::Object(sorted.into_iter().collect())
        }
        other => other,
    }
}

/// The answer of a create whose key binds exactly one operation (card and recipe creates), on its
/// first attempt and on every replay: the stored result, or the stored failure. A failure past the
/// commit was settled by its compensation, so it is final for its key and answers 500
/// `operation_failed`: the create will not complete, and only a new key may try again. A stuck
/// operation is never driven again either (an operator clears it), but what it wrote may exist, so
/// its outcome stays unknown: the plain 500 a client keeps its key for. Not for track or conversation
/// create, whose key steps to a new attempt after a failure.
pub(crate) fn keyed_create_result(outcome: OperationOutcome) -> Result<Value> {
    match outcome {
        OperationOutcome::Succeeded { result }
        | OperationOutcome::SucceededViaCollision { result, .. } => Ok(result),
        OperationOutcome::Failed {
            last_error,
            from_phase,
            last_error_class,
        } => Err(
            match calm_error_from_operation_failure(
                last_error_class.as_deref(),
                last_error,
                from_phase,
            ) {
                CalmError::Internal(message) => CalmError::OperationFailed(message),
                refused => refused,
            },
        ),
        OperationOutcome::Stuck { .. } => {
            Err(CalmError::Internal("operation stuck, see DB".to_string()))
        }
    }
}

/// A keyed card create's answer: the stored card with its runtime projected, 201, or the stored
/// failure as [`keyed_create_result`] reads it.
pub(crate) async fn keyed_card_answer<R: WorkerSessionProjectionRepo + ?Sized>(
    repo: &R,
    outcome: OperationOutcome,
) -> Result<(StatusCode, Json<Card>)> {
    let mut card: Card = serde_json::from_value(keyed_create_result(outcome)?)?;
    project_runtime_into_card_payload(repo, &mut card).await?;
    Ok((StatusCode::CREATED, Json(card)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::PhaseTag;
    use axum::http::{HeaderValue, StatusCode};

    fn parse(value: HeaderValue) -> Result<Option<String>> {
        let mut headers = HeaderMap::new();
        headers.insert("idempotency-key", value);
        parse_idempotency_key_header(&headers)
    }

    #[test]
    fn a_key_up_to_the_cap_is_kept_and_one_byte_more_is_invalid() {
        let longest = "k".repeat(IDEMPOTENCY_KEY_MAX_LEN);
        assert_eq!(
            parse(HeaderValue::from_str(&longest).unwrap()).unwrap(),
            Some(longest.clone())
        );
        // The cap applies to the trimmed key, so surrounding blanks do not count.
        assert_eq!(
            parse(HeaderValue::from_str(&format!(" {longest} ")).unwrap()).unwrap(),
            Some(longest)
        );
        let refused =
            parse(HeaderValue::from_str(&"k".repeat(IDEMPOTENCY_KEY_MAX_LEN + 1)).unwrap())
                .unwrap_err();
        assert_eq!(refused.code(), "idempotency_key_invalid", "{refused:?}");
        assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn a_blank_or_non_ascii_key_is_invalid_and_no_key_is_none() {
        for value in [
            HeaderValue::from_static("   "),
            HeaderValue::from_bytes(b"\xff").unwrap(),
        ] {
            let refused = parse(value).unwrap_err();
            assert_eq!(refused.code(), "idempotency_key_invalid", "{refused:?}");
        }
        assert_eq!(
            parse_idempotency_key_header(&HeaderMap::new()).unwrap(),
            None
        );
    }

    /// A compensated failure is final for its key; a refusal keeps its own code; a stuck operation
    /// may have left its row behind, so it stays the plain 500 a client keeps its key for.
    #[test]
    fn a_failed_operation_is_final_and_a_stuck_one_is_not() {
        let failed = |class: Option<&str>, from_phase| OperationOutcome::Failed {
            last_error: "spawn failed".into(),
            from_phase,
            last_error_class: class.map(str::to_owned),
        };
        let compensated =
            keyed_create_result(failed(Some("internal"), PhaseTag::SpawnStarted)).unwrap_err();
        assert_eq!(compensated.code(), "operation_failed", "{compensated:?}");
        assert_eq!(compensated.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let refused =
            keyed_create_result(failed(Some("not_found"), PhaseTag::Pending)).unwrap_err();
        assert_eq!(refused.code(), "not_found", "{refused:?}");
        let stuck = keyed_create_result(OperationOutcome::Stuck {
            reason: "lease lost".into(),
            from_phase: PhaseTag::SpawnStarted,
        })
        .unwrap_err();
        assert_eq!(stuck.code(), "internal", "{stuck:?}");
    }
}
