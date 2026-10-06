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

/// A refusal an operation's adapter raised before anything was written, as the operation stores it:
/// `(last_error, last_error_class)`. `None` is a fault, not a refusal. [`calm_error_from_operation_failure`]
/// is the inverse. A field violation is stored as its `<field>: <reason>` text under `bad_request`:
/// the stored row has no column for the field, and the text still names it.
pub(crate) fn operation_failure_parts(error: &CalmError) -> Option<(String, &'static str)> {
    match error {
        CalmError::BadRequest(message) => Some((message.clone(), "bad_request")),
        CalmError::InvalidField { field, reason } => {
            Some((format!("{field}: {reason}"), "bad_request"))
        }
        CalmError::InvalidBody(message) => Some((message.clone(), "invalid_body")),
        CalmError::UnsupportedMediaType(message) => {
            Some((message.clone(), "unsupported_media_type"))
        }
        CalmError::NotFound(message) => Some((message.clone(), "not_found")),
        CalmError::Forbidden(message) => Some((message.clone(), "forbidden")),
        CalmError::Conflict(message) => Some((message.clone(), "conflict")),
        CalmError::Unauthorized => Some(("unauthorized".into(), "unauthorized")),
        _ => None,
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
        Some("invalid_body") => CalmError::InvalidBody(last_error),
        Some("unsupported_media_type") => CalmError::UnsupportedMediaType(last_error),
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
/// operation is never driven again either, so every replay answers the same and it is final for its
/// key too. Stuck at `pending` it wrote nothing: its effects commit in the transaction that clears
/// its lease, and only the lease holder marks it stuck. So it answers `operation_failed` as well.
/// Stuck anywhere later, what it made may exist: 500 `operation_stuck`. Not for track or
/// conversation create, whose key steps to a new attempt after a failure.
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
        OperationOutcome::Stuck { reason, from_phase } => Err(if from_phase == PhaseTag::Pending {
            CalmError::OperationFailed(reason)
        } else {
            CalmError::OperationStuck(reason)
        }),
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

    /// Every refusal an adapter can raise is stored as a refusal and answered with its own code and
    /// status again; a field violation keeps its field in the text.
    #[test]
    fn a_stored_refusal_answers_with_the_code_it_was_raised_with() {
        let refusals = [
            CalmError::BadRequest("x".into()),
            CalmError::InvalidField {
                field: "template_input.issue_url".into(),
                reason: "required field is missing".into(),
            },
            CalmError::InvalidBody("x".into()),
            CalmError::UnsupportedMediaType("x".into()),
            CalmError::NotFound("x".into()),
            CalmError::Forbidden("x".into()),
            CalmError::Conflict("x".into()),
            CalmError::Unauthorized,
        ];
        for refusal in refusals {
            let (last_error, class) = operation_failure_parts(&refusal)
                .unwrap_or_else(|| panic!("{refusal:?} must be stored as a refusal"));
            let answered =
                calm_error_from_operation_failure(Some(class), last_error, PhaseTag::SpawnStarted);
            assert_eq!(answered.code(), refusal.code(), "{refusal:?}");
            assert_eq!(answered.status(), refusal.status(), "{refusal:?}");
        }
        let (text, _) = operation_failure_parts(&CalmError::InvalidField {
            field: "config.retries".into(),
            reason: "expected type `integer`".into(),
        })
        .unwrap();
        assert_eq!(text, "config.retries: expected type `integer`");
        assert!(operation_failure_parts(&CalmError::Internal("x".into())).is_none());
    }

    /// A compensated failure is final for its key; a refusal keeps its own code. A stuck operation is
    /// never driven again, so it is final for its key too: stuck at `pending` it wrote nothing and
    /// answers as failed; stuck anywhere later what it made may exist, which `operation_stuck` says.
    #[test]
    fn a_failed_operation_is_final_and_a_stuck_one_answers_by_its_phase() {
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
        let stuck = |from_phase| {
            keyed_create_result(OperationOutcome::Stuck {
                reason: "operation drive failed: lease lost".into(),
                from_phase,
            })
            .unwrap_err()
        };
        let before_commit = stuck(PhaseTag::Pending);
        assert_eq!(
            before_commit.code(),
            "operation_failed",
            "{before_commit:?}"
        );
        assert_eq!(before_commit.status(), StatusCode::INTERNAL_SERVER_ERROR);
        for from_phase in [
            PhaseTag::TxCommitted,
            PhaseTag::AppServerInteract,
            PhaseTag::SpawnStarted,
            PhaseTag::SpawnSucceeded,
            PhaseTag::Parked,
            PhaseTag::Compensating,
            // A row whose detail lost its phase reads as `failed`: past the commit, so maybe made.
            PhaseTag::Failed,
        ] {
            let after_commit = stuck(from_phase);
            assert_eq!(
                after_commit.code(),
                "operation_stuck",
                "{from_phase:?}: {after_commit:?}"
            );
            assert_eq!(after_commit.status(), StatusCode::INTERNAL_SERVER_ERROR);
        }
    }
}
