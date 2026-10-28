//! Exact transaction-local comparison of ingress facts. This is an ephemeral
//! comparison value, never a persisted ledger or a replacement source of authority.
use crate::error::Result;
use sqlx::{Column, Row, Sqlite, Transaction, TypeInfo, ValueRef};

type ExactRows = Vec<Vec<Option<Vec<u8>>>>;

#[derive(Debug, PartialEq)]
pub struct CarrierIngressRevision(Vec<ExactRows>);

pub async fn carrier_ingress_revision_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card: &str,
    track: &str,
    worker: &str,
) -> Result<CarrierIngressRevision> {
    // Whole rows include identity, content and same-sequence revisions. A new
    // column in these owning relations is automatically part of the comparison.
    // All Track events deliberately fence arrivals, including ones not yet
    // eligible for the dispatcher's accepted prefix. No second event-kind list.
    let mut relations = Vec::new();
    for (query, key) in [
        (
            "SELECT * FROM events WHERE scope_track=?1 ORDER BY id",
            track,
        ),
        (
            "SELECT * FROM conversation_presentations WHERE card_id=?1 ORDER BY sequence",
            card,
        ),
        (
            "SELECT * FROM harness_items WHERE card_id=?1 ORDER BY id",
            card,
        ),
        (
            "SELECT * FROM conversation_messages WHERE card_id=?1 ORDER BY id",
            card,
        ),
        (
            concat!(
                "SELECT r.* FROM conversation_message_revisions r JOIN conversation_messages m ON m.id=r.message_id WHERE ",
                "m.card_id=?1 ORDER BY r.message_id,r.revision"
            ),
            card,
        ),
        (
            "SELECT * FROM conversation_turns WHERE card_id=?1 ORDER BY id",
            card,
        ),
        (
            "SELECT * FROM conversation_input_submissions WHERE card_id=?1 ORDER BY id",
            card,
        ),
        (
            concat!(
                "SELECT i.* FROM conversation_submission_inputs i JOIN conversation_input_submissions s ON ",
                "s.id=i.submission_id WHERE s.card_id=?1 ORDER BY i.submission_id,i.input_position"
            ),
            card,
        ),
        (
            "SELECT * FROM conversation_input_retry_relations WHERE card_id=?1 ORDER BY retry_intent_id",
            card,
        ),
        (
            concat!(
                "SELECT b.* FROM worker_session_backends b JOIN worker_sessions w ON w.id=b.worker_session_id WHERE ",
                "w.card_id=?1 ORDER BY b.worker_session_id"
            ),
            card,
        ),
        (
            "SELECT * FROM acp_normal_joins WHERE worker_session_id=?1 ORDER BY client_id",
            worker,
        ),
        (
            "SELECT * FROM acp_managed_sessions WHERE worker_session_id=?1",
            worker,
        ),
        (
            "SELECT * FROM acp_submissions WHERE worker_session_id=?1 ORDER BY client_id",
            worker,
        ),
        (
            "SELECT * FROM worker_sessions WHERE card_id=?1 ORDER BY id",
            card,
        ),
        (
            "SELECT kind,role,payload,session_id,track_id FROM cards WHERE id=?1",
            card,
        ),
        ("SELECT root_session_id FROM tracks WHERE id=?1", track),
    ] {
        let mut rows = Vec::new();
        for row in sqlx::query(query).bind(key).fetch_all(&mut **tx).await? {
            let mut values = Vec::new();
            for column in row.columns() {
                let i = column.ordinal();
                let raw = row.try_get_raw(i)?;
                let value = if raw.is_null() {
                    None
                } else {
                    let mut bytes = raw.type_info().name().as_bytes().to_vec();
                    bytes.push(0);
                    match raw.type_info().name() {
                        "INTEGER" => bytes.extend(row.try_get::<i64, _>(i)?.to_le_bytes()),
                        "REAL" => bytes.extend(row.try_get::<f64, _>(i)?.to_bits().to_le_bytes()),
                        "BLOB" => bytes.extend(row.try_get::<Vec<u8>, _>(i)?),
                        _ => bytes.extend(row.try_get::<String, _>(i)?.as_bytes()),
                    }
                    Some(bytes)
                };
                values.push(value);
            }
            rows.push(values);
        }
        relations.push(rows);
    }
    Ok(CarrierIngressRevision(relations))
}
