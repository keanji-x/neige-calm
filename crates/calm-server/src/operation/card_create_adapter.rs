//! `card-create`: the one write of `POST /api/tracks/:track_id/cards`, a card that owns no
//! runtime (a plugin `ui://` card, or a direct create). It is an `operations` row only so the
//! route's `Idempotency-Key` binds the card it made: a retry under the key is answered from the
//! stored row instead of making a second card. The route validates the body and calls any plugin
//! tool before it commits (`OperationRuntime::commit_keyed`); this writes the card and its
//! `card.added` in that one transaction, and has no side effect.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::card_role_cache::CardRoleCache;
use crate::db::RouteRepo;
use crate::db::sqlite::{append_decision_event_in_tx, card_create_with_id_tx};
use crate::error::{CalmError, Result};
use crate::event::{BroadcastEnvelope, Event, SYNC_EVENT_VERSION};
use crate::ids::{ActorId, CardId, TrackId};
use crate::model::{CardRole, NewCard, new_id};
use crate::routes::cards::card_scope_tx;
use std::sync::Arc;

use super::{Operation, Tx, TxOnlyAdapter, TxOutput};

pub const CARD_CREATE: &str = "card-create";

/// The card exactly as it is written. The route builds it after validation (and after the plugin
/// tool answered); nothing here is read from mutable state.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CardCreateOperationPayload {
    /// `Plugin(<id>)` for a card a plugin tool made, so plugins cannot spoof their actor.
    pub actor: ActorId,
    /// `user_tool_call:<tool>` for a tool-made card; `None` for a direct create.
    pub correlation: Option<String>,
    pub track_id: String,
    pub kind: String,
    pub sort: Option<f64>,
    pub payload: Value,
    pub title: Option<String>,
}

#[derive(Clone)]
pub struct CardCreateAdapter {
    repo: Arc<dyn RouteRepo>,
    card_role_cache: CardRoleCache,
}

impl CardCreateAdapter {
    pub fn new(repo: Arc<dyn RouteRepo>, card_role_cache: CardRoleCache) -> Self {
        Self {
            repo,
            card_role_cache,
        }
    }
}

#[async_trait]
impl TxOnlyAdapter for CardCreateAdapter {
    fn kind(&self) -> &'static str {
        CARD_CREATE
    }

    async fn validate(&self, input: &Value) -> Result<()> {
        let payload: CardCreateOperationPayload = serde_json::from_value(input.clone())?;
        if self.repo.track_get(&payload.track_id).await?.is_none() {
            return Err(CalmError::NotFound(format!("track {}", payload.track_id)));
        }
        Ok(())
    }

    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        _op: &Operation,
    ) -> Result<TxOutput> {
        let payload: CardCreateOperationPayload = serde_json::from_value(input.clone())?;
        let card_id = new_id();
        let track_id = TrackId::from(payload.track_id);
        let scope = card_scope_tx(tx, CardId::from(card_id.clone()), track_id.clone()).await?;
        // A user-driven create mints a user-deletable Worker card; `false` is for kernel-owned cards.
        let card = card_create_with_id_tx(
            tx,
            card_id,
            NewCard {
                track_id,
                kind: payload.kind,
                sort: payload.sort,
                payload: payload.payload,
                title: payload.title,
            },
            CardRole::Worker,
            true,
            &self.card_role_cache,
        )
        .await?;
        let event = Event::CardAdded(card.clone());
        let event_id = append_decision_event_in_tx(
            tx,
            &payload.actor,
            &scope,
            payload.correlation.as_deref(),
            &event,
        )
        .await?;
        let mut output = TxOutput::new(
            "card",
            Some(card.id.to_string()),
            serde_json::to_value(&card)?,
        );
        output.post_commit_events.push(BroadcastEnvelope {
            id: event_id,
            event_version: SYNC_EVENT_VERSION,
            actor: payload.actor,
            scope,
            event,
        });
        Ok(output)
    }
}
