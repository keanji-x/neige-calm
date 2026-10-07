//! The kernel owns dispatched queue batches independently of native prompt text.
use super::{HarnessPhaseTag, HarnessSnapshot, QueueEntry};
use crate::error::{CalmError, Result};
use serde_json::{Value, json};

pub(crate) fn freeze(entries: &[QueueEntry]) -> Value {
    Value::Array(entries.iter().map(|entry| {
        let kind=match entry {QueueEntry::User{..}=>"user",QueueEntry::LegacyUser{..}=>"legacy-user",QueueEntry::System{..}=>"system"};
        let user=entry.user_view();
        json!({"kind":kind,"id":entry.id(),"observation":entry.observation(),"envelopeId":entry.envelope_id(),"messageIds":entry.message_ids(),"attachments":entry.attachments(),"rev":user.as_ref().map(|view|view.rev),"queuedAtMs":user.as_ref().map(|view|view.queued_at_ms)})
    }).collect())
}

pub(crate) fn retire(snapshot: &mut HarnessSnapshot, client: &str, claims: &Value) -> Result<bool> {
    if snapshot.phase != HarnessPhaseTag::IssuingTurn
        || snapshot.projection_client_id.as_ref().map(|id| id.as_str()) != Some(client)
    {
        return Ok(false);
    }
    let claims = claims
        .as_array()
        .filter(|claims| !claims.is_empty())
        .ok_or_else(|| {
            CalmError::Conflict("Dispatched input has no queue ownership proof".into())
        })?;
    let entries = snapshot.pending_entries();
    if entries.len() < claims.len()
        || freeze(&entries[..claims.len()]) != Value::Array(claims.clone())
    {
        return Err(CalmError::Conflict(
            "Dispatched queue ownership changed; refusing to transfer or discard input".into(),
        ));
    }
    snapshot.set_pending_entries(entries.into_iter().skip(claims.len()).collect());
    snapshot.projection_client_id = None;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dispatched_batch_retirement_preserves_later_equal_text_by_identity() {
        let original = QueueEntry::user_message("same text".into(), None, Vec::new());
        let later = QueueEntry::user_message("same text".into(), None, Vec::new());
        let client = original.id().unwrap().clone();
        let proof = freeze(std::slice::from_ref(&original));
        let mut snapshot = HarnessSnapshot::initial(0, vec![original, later.clone()]);
        snapshot.phase = HarnessPhaseTag::IssuingTurn;
        snapshot.projection_client_id = Some(client.clone());
        assert!(retire(&mut snapshot, client.as_str(), &proof).unwrap());
        assert_eq!(snapshot.pending_entries(), vec![later]);
    }
    #[test]
    fn mismatched_claim_does_not_discard_unrelated_input() {
        let original = QueueEntry::user_message("old".into(), None, Vec::new());
        let client = original.id().unwrap().clone();
        let proof = freeze(&[original]);
        let current = QueueEntry::user_message("new".into(), None, Vec::new());
        let mut snapshot = HarnessSnapshot::initial(0, vec![current.clone()]);
        snapshot.phase = HarnessPhaseTag::IssuingTurn;
        snapshot.projection_client_id = Some(client.clone());
        assert!(retire(&mut snapshot, client.as_str(), &proof).is_err());
        assert_eq!(snapshot.pending_entries(), vec![current]);
    }
}
