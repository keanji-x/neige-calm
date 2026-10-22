use super::*;
use objective::ids::ObjectiveKey;
use sqlx::Row;

#[derive(Clone, Debug, PartialEq)]
pub struct LoggedEvent {
    pub track_id: TrackId,
    pub seq: LogSeq,
    pub event: Event,
    pub source: InputSource,
    pub appended_at_ms: i64,
}

pub(super) fn kind(event: &Event) -> &'static str {
    match event {
        Event::DefinitionChanged { .. } => "definition_changed",
        Event::ApprovalGranted { .. } => "approval_granted",
        Event::Requested { .. } => "requested",
        Event::CheckFinished { .. } => "check_finished",
        Event::RevisionDelivered { .. } => "revision_delivered",
        Event::Settled { .. } => "settled",
        Event::Closed { .. } => "closed",
        Event::HostIdle { .. } => "host_idle",
        Event::Usage { .. } => "usage",
    }
}
fn objective_of(event: &Event) -> Option<&ObjectiveKey> {
    match event {
        Event::DefinitionChanged { objective } => Some(&objective.key),
        Event::ApprovalGranted { objective, .. }
        | Event::Requested { objective, .. }
        | Event::CheckFinished { objective, .. }
        | Event::Settled { objective, .. }
        | Event::Closed { objective } => Some(objective),
        Event::RevisionDelivered { .. } | Event::HostIdle { .. } | Event::Usage { .. } => None,
    }
}
pub(super) fn payload(event: &Event) -> Result<String> {
    let json = serde_json::to_string(event).map_err(|e| TruthError::BadRequest(e.to_string()))?;
    // serde_json encodes non-finite floats as null. Never persist a lossy event.
    let decoded = serde_json::from_str::<Event>(&json);
    if decoded.as_ref().ok() != Some(event) {
        return Err(TruthError::BadRequest(
            "objective event does not round-trip through JSON",
        ));
    }
    Ok(json)
}

pub(super) async fn dedupe_tx(
    tx: &mut Transaction<'_, Sqlite>,
    authorized: &authorize::AuthorizedInput<'_>,
) -> Result<Option<LogSeq>> {
    let input = authorized.input();
    let (source_kind, source_key) = input.source.identity()?;
    let row:Option<(i64,String)>=sqlx::query_as("SELECT seq,payload_json FROM objective_events WHERE track_id=? AND source_kind=? AND source_key=? AND kind=?")
        .bind(input.track_id.as_str()).bind(source_kind).bind(source_key).bind(kind(&input.event)).fetch_optional(&mut **tx).await?;
    match row {
        Some((seq, json)) if json == payload(&input.event)? => {
            Ok(Some(LogSeq(u64::try_from(seq).map_err(internal)?)))
        }
        Some(_) => Err(TruthError::Conflict(
            "objective source identity already has a different payload",
        )),
        None => Ok(None),
    }
}

pub(super) async fn append_tx(
    tx: &mut Transaction<'_, Sqlite>,
    authorized: &authorize::AuthorizedInput<'_>,
    seq: LogSeq,
    at_ms: i64,
) -> Result<()> {
    let input = authorized.input();
    let (source_kind, source_key) = input.source.identity()?;
    sqlx::query(
        "INSERT INTO objective_events(track_id,seq,schema_version,kind,objective_key,payload_json,
        source_kind,source_key,appended_at_ms) VALUES(?,?,?,?,?,?,?,?,?)",
    )
    .bind(input.track_id.as_str())
    .bind(sql_seq(seq)?)
    .bind(i64::from(objective::engine::EVENT_VERSION))
    .bind(kind(&input.event))
    .bind(objective_of(&input.event).map(ObjectiveKey::as_str))
    .bind(payload(&input.event)?)
    .bind(source_kind)
    .bind(source_key)
    .bind(at_ms)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub(super) async fn read_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &TrackId,
) -> Result<Vec<LoggedEvent>> {
    let rows = sqlx::query("SELECT e.*,t.area_id FROM objective_events e JOIN tracks t ON t.id=e.track_id WHERE e.track_id=? ORDER BY e.seq")
        .bind(track.as_str())
        .fetch_all(&mut **tx)
        .await?;
    decode_rows(&rows, track)
}

pub(super) fn decode_rows(
    rows: &[sqlx::sqlite::SqliteRow],
    track: &TrackId,
) -> Result<Vec<LoggedEvent>> {
    let mut result = vec![];
    for row in rows {
        let seq: i64 = row.try_get("seq")?;
        if seq != result.len() as i64 + 1 {
            return Err(internal("non-dense objective log"));
        }
        if row.try_get::<i64, _>("schema_version")? != i64::from(objective::engine::EVENT_VERSION) {
            return Err(internal("unknown event version"));
        }
        let event: Event = serde_json::from_str(row.try_get("payload_json")?).map_err(internal)?;
        if row.try_get::<&str, _>("kind")? != kind(&event)
            || row.try_get::<Option<&str>, _>("objective_key")?
                != objective_of(&event).map(ObjectiveKey::as_str)
        {
            return Err(internal("event envelope mismatch"));
        }
        let source_key: &str = row.try_get("source_key")?;
        let source = match row.try_get::<&str, _>("source_kind")? {
            "report_block" => {
                let (report_card_id, doc_rev, block_id) =
                    serde_json::from_str(source_key).map_err(internal)?;
                InputSource::ReportBlock {
                    report_card_id,
                    doc_rev,
                    block_id,
                }
            }
            "intent_job" => InputSource::IntentJob {
                intent: serde_json::from_str(source_key).map_err(internal)?,
            },
            "planner_call" => {
                let (session, id): (WorkerSessionId, String) =
                    serde_json::from_str(source_key).map_err(internal)?;
                InputSource::Planner {
                    principal: Principal::Agent {
                        session_id: session,
                        track_id: track.clone(),
                        area_id: row.try_get::<String, _>("area_id")?.into(),
                    },
                    request_id: PlannerRequestId::new(id),
                }
            }
            "harness_turn" => {
                let (session, id): (WorkerSessionId, String) =
                    serde_json::from_str(source_key).map_err(internal)?;
                InputSource::HarnessTurn {
                    session_id: session,
                    turn_id: id,
                }
            }
            _ => return Err(internal("unknown source kind")),
        };
        source.identity().map_err(internal)?;
        result.push(LoggedEvent {
            track_id: track.clone(),
            seq: LogSeq(seq as u64),
            event,
            source,
            appended_at_ms: row.try_get("appended_at_ms")?,
        });
    }
    Ok(result)
}
