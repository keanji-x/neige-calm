use super::*;
use objective::standard::{StandardHash, standard_hash};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntentId {
    pub track_id: TrackId,
    pub seq: LogSeq,
    pub ordinal: u32,
}
impl std::fmt::Display for IntentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "objective-intent:{}:{}:{}",
            self.track_id, self.seq.0, self.ordinal
        )
    }
}
/// Kind-specific required data cannot be omitted by callers.
#[derive(Clone, Debug, PartialEq)]
pub enum StoredEffect {
    RunCheck { standard_hash: StandardHash },
    Execute { target_session_id: WorkerSessionId },
    Notify,
}
#[derive(Clone, Debug, PartialEq)]
pub struct StoredIntent {
    pub id: IntentId,
    pub intent: Intent,
    pub effect: StoredEffect,
    pub created_at_ms: i64,
}

fn effect_key(intent: &Intent, effect: &StoredEffect, seq: LogSeq, ordinal: u32) -> Result<String> {
    let value = match (intent, effect) {
        (
            Intent::RunCheck {
                objective,
                evaluation,
                check,
                revision,
                sample,
            },
            StoredEffect::RunCheck { standard_hash },
        ) => serde_json::json!([
            "run_check",
            objective,
            standard_hash,
            evaluation,
            check,
            revision,
            sample
        ]),
        (Intent::Execute { call }, StoredEffect::Execute { .. }) => {
            serde_json::json!(["execute", call.idempotency_key()])
        }
        (Intent::Notify { .. }, StoredEffect::Notify) => {
            serde_json::json!(["notify", seq.0, ordinal])
        }
        _ => return Err(internal("intent binding kind mismatch")),
    };
    Ok(serde_json::to_string(&value)?)
}

pub(super) async fn insert_tx(
    tx: &mut Transaction<'_, Sqlite>,
    authorized: &authorize::AuthorizedInput<'_>,
    before: &State,
    seq: LogSeq,
    intents: Vec<Intent>,
    at_ms: i64,
) -> Result<Vec<StoredIntent>> {
    let input = authorized.input();
    let mut stored = vec![];
    for (ordinal, intent) in intents.into_iter().enumerate() {
        let ordinal = u32::try_from(ordinal).map_err(internal)?;
        let (kind, effect) = match &intent {
            Intent::RunCheck { objective, .. } => {
                let definition = &before
                    .objectives
                    .get(objective)
                    .ok_or_else(|| internal("RunCheck without pre-react definition"))?
                    .definition;
                (
                    "run_check",
                    StoredEffect::RunCheck {
                        standard_hash: standard_hash(definition),
                    },
                )
            }
            Intent::Execute { .. } => {
                let InputSource::HarnessTurn { session_id, .. } = &input.source else {
                    return Err(internal("Execute without HarnessTurn"));
                };
                (
                    "execute",
                    StoredEffect::Execute {
                        target_session_id: session_id.clone(),
                    },
                )
            }
            Intent::Notify { .. } => ("notify", StoredEffect::Notify),
        };
        let key = effect_key(&intent, &effect, seq, ordinal)?;
        let hash = match &effect {
            StoredEffect::RunCheck { standard_hash } => Some(standard_hash.as_str()),
            _ => None,
        };
        let session = match &effect {
            StoredEffect::Execute { target_session_id } => Some(target_session_id.as_str()),
            _ => None,
        };
        sqlx::query("INSERT INTO objective_intents(track_id,event_seq,ordinal,kind,intent_json,effect_key,standard_hash,
        target_session_id,created_at_ms) VALUES(?,?,?,?,?,?,?,?,?)")
            .bind(input.track_id.as_str()).bind(sql_seq(seq)?).bind(i64::from(ordinal)).bind(kind).bind(serde_json::to_string(&intent)?)
            .bind(key).bind(hash).bind(session).bind(at_ms).execute(&mut **tx).await.map_err(internal)?;
        stored.push(StoredIntent {
            id: IntentId {
                track_id: input.track_id.clone(),
                seq,
                ordinal,
            },
            intent,
            effect,
            created_at_ms: at_ms,
        });
    }
    Ok(stored)
}
