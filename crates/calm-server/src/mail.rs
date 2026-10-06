//! Mail between the Tracks of one Area (#2130, `docs/architecture/2130-track-mail.md`). A send
//! stores one `mails` row and, in the same transaction, the `track.wake_requested` that wakes the
//! recipient's Planner; the recipient's first `cat` stamps `read_at`. A causal hop count, computed
//! from the caller's current turn, refuses the seventh hop.

use std::sync::{Arc, Mutex};

use calm_truth::decision_gate::{GateDecision, PrincipalDecisionGate};
use calm_types::observation::MAIL_WAKE_SOURCE;
use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};

use crate::db::sqlite::track_find_tx;
use crate::db::{write_in_tx_typed, write_with_events_typed};
use crate::error::CalmError;
use crate::event::{Event, EventScope};
use crate::harness::turn_input::{self, TurnInput};
use crate::ids::{ActorId, TrackId};
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{AppContext, ToolCallIdentity};
use crate::model::{new_id, now_ms};

pub const TOOL_MAIL_SEND: &str = "neige_mail_send";
pub const TOOL_MAIL_LS: &str = "neige_mail_ls";
pub const TOOL_MAIL_CAT: &str = "neige_mail_cat";

/// The highest hop a mail may carry; a send that would get a higher one is refused.
pub const MAX_HOP: i64 = 6;
/// Rows per `ls` page.
pub const PAGE: i64 = 50;

/// Who a send goes to: a Track of the caller's Area, or the sender of a mail the caller got.
#[derive(Clone, Debug)]
pub enum Recipient {
    Track(String),
    Reply(String),
}

#[derive(Clone, Debug)]
pub struct SendRequest {
    pub to: Recipient,
    pub summary: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sent {
    pub mail_id: String,
    pub hop: i64,
}

/// `n/6`, the one spelling of a hop in every result.
pub fn hop_label(hop: i64) -> String {
    format!("{hop}/{MAX_HOP}")
}

/// A refusal: its JSON-RPC code, the machine kind in `data.refusal`, and the message.
#[derive(Debug)]
struct Refusal {
    code: i64,
    kind: &'static str,
    message: String,
}

impl Refusal {
    fn into_rpc(self) -> RpcError {
        let mut error = RpcError::custom(self.code, self.message);
        error.data = Some(json!({ "refusal": self.kind }));
        error
    }
}

/// A refusal decided inside the write transaction: the transaction carries a `CalmError`, so the
/// refusal travels beside it and the error only rolls the transaction back.
#[derive(Clone, Default)]
struct RefusalSlot(Arc<Mutex<Option<Refusal>>>);

impl RefusalSlot {
    fn refuse(&self, code: i64, kind: &'static str, message: String) -> CalmError {
        *self.0.lock().expect("refusal slot") = Some(Refusal {
            code,
            kind,
            message,
        });
        CalmError::Conflict("mail refused".into())
    }

    fn into_rpc(self, tool: &str, error: CalmError) -> RpcError {
        match self.0.lock().expect("refusal slot").take() {
            Some(refusal) => refusal.into_rpc(),
            None => RpcError::internal(format!("{tool}: {error}")),
        }
    }
}

/// A refusal decided before any transaction.
pub fn invalid(tool: &str, kind: &'static str, message: &str) -> RpcError {
    Refusal {
        code: RpcError::INVALID_PARAMS,
        kind,
        message: format!("{tool}: {message}"),
    }
    .into_rpc()
}

fn caller_track(identity: &ToolCallIdentity, tool: &str) -> Result<String, RpcError> {
    identity.track_id.clone().ok_or_else(|| {
        Refusal {
            code: -32403,
            kind: "no_track",
            message: format!("{tool}: this caller has no track"),
        }
        .into_rpc()
    })
}

/// D16: the caller session is still the active authority of a card on its Track, in the caller's tx.
async fn session_is_active(
    tx: &mut Transaction<'_, Sqlite>,
    identity: &ToolCallIdentity,
    track: &str,
) -> Result<bool, CalmError> {
    let Some(principal) = identity.to_principal() else {
        return Ok(false);
    };
    let decision = PrincipalDecisionGate::new(principal)
        .decide_recorder(tx, &TrackId::from(track.to_string()))
        .await?;
    Ok(matches!(decision, GateDecision::Allow))
}

/// §5: 1 in a turn the user spoke in, else 1 + the highest hop among the mails to this Track read
/// since the turn started and the mail replied to.
async fn next_hop(
    tx: &mut Transaction<'_, Sqlite>,
    track: &str,
    turn: TurnInput,
    replied_hop: Option<i64>,
) -> Result<i64, CalmError> {
    if turn.has_user_segment {
        return Ok(1);
    }
    let read: Option<i64> =
        sqlx::query_scalar("SELECT MAX(hop) FROM mails WHERE to_track_id = ?1 AND read_at >= ?2")
            .bind(track)
            .bind(turn.created_at_ms)
            .fetch_one(&mut **tx)
            .await?;
    Ok(1 + read.unwrap_or(0).max(replied_hop.unwrap_or(0)))
}

/// `neige_mail_send`: one mail row and its wake, in one transaction, after every check of §6.
pub async fn send(
    ctx: &AppContext,
    identity: &ToolCallIdentity,
    request: SendRequest,
) -> Result<Sent, RpcError> {
    const TOOL: &str = TOOL_MAIL_SEND;
    let caller = caller_track(identity, TOOL)?;
    let slot = RefusalSlot::default();
    let refusals = slot.clone();
    let identity = identity.clone();
    let result = write_with_events_typed(
        ctx.repo.as_ref(),
        ActorId::Kernel,
        None,
        &ctx.events,
        &ctx.write,
        move |tx| {
            Box::pin(async move {
                let slot = refusals;
                if !session_is_active(tx, &identity, &caller).await? {
                    return Err(slot.refuse(
                        -32403,
                        "session_inactive",
                        format!("{TOOL}: this Planner session is no longer active; nothing was sent"),
                    ));
                }
                let (to_track, reply_to, replied_hop) = match request.to {
                    Recipient::Track(track) => (track, None, None),
                    Recipient::Reply(mail_id) => {
                        let replied: Option<(String, i64)> = sqlx::query_as(
                            "SELECT from_track_id, hop FROM mails WHERE id = ?1 AND to_track_id = ?2",
                        )
                        .bind(&mail_id)
                        .bind(&caller)
                        .fetch_optional(&mut **tx)
                        .await?;
                        let Some((from, hop)) = replied else {
                            return Err(slot.refuse(
                                -32404,
                                "unknown_mail",
                                format!(
                                    "{TOOL}: no mail {mail_id} addressed to this track; \
                                     list yours with neige mail ls"
                                ),
                            ));
                        };
                        (from, Some(mail_id), Some(hop))
                    }
                };
                let sender = track_find_tx(tx, &caller).await?.ok_or_else(|| {
                    CalmError::Internal(format!("{TOOL}: the caller's track {caller} is gone"))
                })?;
                let recipient = track_find_tx(tx, &to_track)
                    .await?
                    .filter(|track| track.area_id == sender.area_id)
                    .ok_or_else(|| {
                        slot.refuse(
                            -32404,
                            "unknown_track",
                            format!(
                                "{TOOL}: no track {to_track} in this area; \
                                 list the area's tracks with neige_area_ls"
                            ),
                        )
                    })?;
                if recipient.id.as_str() == caller {
                    return Err(slot.refuse(
                        RpcError::INVALID_PARAMS,
                        "self",
                        format!("{TOOL}: track {to_track} is this track; mail another track of the area"),
                    ));
                }
                if recipient.closed_at.is_some() {
                    return Err(slot.refuse(
                        -32409,
                        "closed",
                        format!("{TOOL}: track {to_track} is closed; hand off with neige_user_ask"),
                    ));
                }
                if recipient.purpose.as_deref() == Some(crate::AREA_CHAT_PURPOSE) {
                    return Err(slot.refuse(
                        -32409,
                        "no_planner",
                        format!("{TOOL}: track {to_track} has no Planner; hand off with neige_user_ask"),
                    ));
                }
                if crate::managed_track::reports_only_track(&mut **tx, &to_track).await? {
                    return Err(slot.refuse(
                        -32409,
                        "reports_only",
                        format!("{TOOL}: track {to_track} takes no mail; hand off with neige_user_ask"),
                    ));
                }
                let Some(turn) =
                    turn_input::latest(tx, &identity.card_id, &identity.session_id).await?
                else {
                    return Err(slot.refuse(
                        -32409,
                        "turn_unrecorded",
                        format!(
                            "{TOOL}: this turn's input is not recorded; send again from your next turn"
                        ),
                    ));
                };
                let hop = next_hop(tx, &caller, turn, replied_hop).await?;
                if hop > MAX_HOP {
                    return Err(slot.refuse(
                        -32409,
                        "hop_limit",
                        format!("{TOOL}: hop 6/6 reached — hand off with neige_user_ask"),
                    ));
                }
                let mail_id = new_id();
                sqlx::query(
                    "INSERT INTO mails (id, from_track_id, to_track_id, reply_to, summary, text, hop, \
                     sent_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                )
                .bind(&mail_id)
                .bind(&caller)
                .bind(&to_track)
                .bind(&reply_to)
                .bind(&request.summary)
                .bind(&request.text)
                .bind(hop)
                .bind(now_ms())
                .execute(&mut **tx)
                .await?;
                let event = Event::TrackWakeRequested {
                    track_id: recipient.id.clone(),
                    source: MAIL_WAKE_SOURCE.into(),
                    key: mail_id.clone(),
                    text: wake_line(sender.id.as_str(), &sender.title, &request.summary, &mail_id),
                };
                let scope = EventScope::Track {
                    track: recipient.id,
                    area: recipient.area_id,
                };
                Ok((Sent { mail_id, hop }, vec![(scope, event)]))
            })
        },
    )
    .await;
    match result {
        Ok((sent, _)) => Ok(sent),
        Err(error) => Err(slot.into_rpc(TOOL, error)),
    }
}

/// §4.5: the one line the recipient's Planner is woken with; no body, no hop. A title's line
/// breaks become one space; an untitled sender is named by its track id.
pub fn wake_line(sender_id: &str, sender_title: &str, summary: &str, mail_id: &str) -> String {
    let title = sender_title
        .split(['\r', '\n'])
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let name = if title.trim().is_empty() {
        sender_id
    } else {
        title.as_str()
    };
    format!("\"{name}\": {summary} — neige mail cat {mail_id}")
}

/// One mail row, from the caller Track's side: the other Track and its title.
#[derive(sqlx::FromRow)]
struct MailRow {
    id: String,
    from_track_id: String,
    to_track_id: String,
    reply_to: Option<String>,
    summary: String,
    text: String,
    hop: i64,
    sent_at: i64,
    read_at: Option<i64>,
    title: String,
}

const MAIL_COLUMNS: &str = "m.id, m.from_track_id, m.to_track_id, m.reply_to, m.summary, m.text, \
     m.hop, m.sent_at, m.read_at, t.title";
/// The other Track of a mail, seen from `?1`.
const OTHER_TRACK_JOIN: &str = "JOIN tracks t ON t.id = \
     CASE WHEN m.to_track_id = ?1 THEN m.from_track_id ELSE m.to_track_id END";

impl MailRow {
    fn summary_json(&self, caller: &str) -> serde_json::Map<String, Value> {
        let incoming = self.to_track_id == caller;
        let other = if incoming {
            &self.from_track_id
        } else {
            &self.to_track_id
        };
        let state = if self.read_at.is_some() {
            "read"
        } else {
            "unread"
        };
        let row = json!({
            "mail_id": self.id,
            "direction": if incoming { "in" } else { "out" },
            "track_id": other,
            "title": self.title,
            "summary": self.summary,
            "hop": hop_label(self.hop),
            "state": state,
            "sent_at": crate::time_format::at(self.sent_at),
            "read_at": crate::time_format::at_opt(self.read_at),
        });
        let Value::Object(row) = row else {
            unreachable!("json! of an object literal")
        };
        row
    }
}

/// `neige_mail_ls`: this Track's mail, newest first, [`PAGE`] per page. Stamps nothing.
pub async fn ls(
    ctx: &AppContext,
    identity: &ToolCallIdentity,
    cursor: Option<&str>,
) -> Result<Value, RpcError> {
    const TOOL: &str = TOOL_MAIL_LS;
    let caller = caller_track(identity, TOOL)?;
    let after = cursor
        .map(|cursor| {
            cursor
                .split_once(':')
                .and_then(|(at, id)| Some((at.parse::<i64>().ok()?, id.to_string())))
                .ok_or_else(|| {
                    invalid(
                        TOOL,
                        "cursor",
                        &format!("cursor `{cursor}` is not a next_cursor this tool returned"),
                    )
                })
        })
        .transpose()?;
    let pool = ctx
        .sqlite_pool
        .as_ref()
        .ok_or_else(|| RpcError::internal(format!("{TOOL}: requires a sqlite-backed repo")))?;
    let (after_at, after_id) = after.unzip();
    let mut rows: Vec<MailRow> = sqlx::query_as(&format!(
        "SELECT {MAIL_COLUMNS} FROM mails m {OTHER_TRACK_JOIN} \
         WHERE (m.to_track_id = ?1 OR m.from_track_id = ?1) \
           AND (?2 IS NULL OR m.sent_at < ?2 OR (m.sent_at = ?2 AND m.id < ?3)) \
         ORDER BY m.sent_at DESC, m.id DESC LIMIT ?4"
    ))
    .bind(&caller)
    .bind(after_at)
    .bind(after_id)
    .bind(PAGE + 1)
    .fetch_all(pool)
    .await
    .map_err(|e| RpcError::internal(format!("{TOOL}: {e}")))?;
    let more = rows.len() > PAGE as usize;
    rows.truncate(PAGE as usize);
    let next_cursor = match rows.last() {
        Some(last) if more => json!(format!("{}:{}", last.sent_at, last.id)),
        _ => Value::Null,
    };
    let mails: Vec<Value> = rows
        .iter()
        .map(|row| Value::Object(row.summary_json(&caller)))
        .collect();
    Ok(json!({ "mails": mails, "next_cursor": next_cursor }))
}

/// `neige_mail_cat`: one mail to or from this Track. The recipient's first cat stamps `read_at`;
/// `next_hop` is what an allowed new, non-reply send from this turn would get; it is null
/// with `refused: true` at the hop limit, or `refused: false` when no turn is recorded.
pub async fn cat(
    ctx: &AppContext,
    identity: &ToolCallIdentity,
    mail_id: &str,
) -> Result<Value, RpcError> {
    const TOOL: &str = TOOL_MAIL_CAT;
    let caller = caller_track(identity, TOOL)?;
    let slot = RefusalSlot::default();
    let refusals = slot.clone();
    let identity = identity.clone();
    let mail_id = mail_id.to_string();
    let result = write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let slot = refusals;
            let select = format!(
                "SELECT {MAIL_COLUMNS} FROM mails m {OTHER_TRACK_JOIN} \
                 WHERE m.id = ?2 AND (m.to_track_id = ?1 OR m.from_track_id = ?1)"
            );
            let found: Option<MailRow> = sqlx::query_as(&select)
                .bind(&caller)
                .bind(&mail_id)
                .fetch_optional(&mut **tx)
                .await?;
            if found.is_none() {
                return Err(slot.refuse(
                    -32404,
                    "unknown_mail",
                    format!(
                        "{TOOL}: no mail {mail_id} to or from this track; list them with neige mail ls"
                    ),
                ));
            }
            if !session_is_active(tx, &identity, &caller).await? {
                return Err(slot.refuse(
                    -32403,
                    "session_inactive",
                    format!("{TOOL}: this Planner session is no longer active"),
                ));
            }
            // The receipt: only the recipient's first cat stamps; a later one keeps the first time.
            sqlx::query(
                "UPDATE mails SET read_at = ?3 WHERE id = ?1 AND to_track_id = ?2 AND read_at IS NULL",
            )
            .bind(&mail_id)
            .bind(&caller)
            .bind(now_ms())
            .execute(&mut **tx)
            .await?;
            let row: MailRow = sqlx::query_as(&select)
                .bind(&caller)
                .bind(&mail_id)
                .fetch_one(&mut **tx)
                .await?;
            let next = match turn_input::latest(tx, &identity.card_id, &identity.session_id).await? {
                Some(turn) => Some(next_hop(tx, &caller, turn, None).await?),
                None => None,
            };
            let mut mail = row.summary_json(&caller);
            mail.insert("text".into(), json!(row.text));
            mail.insert("reply_to".into(), json!(row.reply_to));
            mail.insert("refused".into(), json!(next.is_some_and(|hop| hop > MAX_HOP)));
            mail.insert(
                "next_hop".into(),
                json!(next.filter(|hop| *hop <= MAX_HOP).map(hop_label)),
            );
            Ok(Value::Object(mail))
        })
    })
    .await;
    result.map_err(|error| slot.into_rpc(TOOL, error))
}
