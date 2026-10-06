//! `track-recipe-create`: the one write of `POST /api/track-recipes`. It is an `operations` row
//! only so the route's `Idempotency-Key` binds the recipe it made: a retry under the key is
//! answered with that recipe instead of saving a second one. The route normalizes and validates
//! the body before it commits (`OperationRuntime::commit_keyed`); this inserts the recipe in that
//! one transaction, with no side effect.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::db::sqlite::track_recipe_create_tx;
use crate::error::Result;

use super::{Operation, Tx, TxOnlyAdapter, TxOutput};

pub const TRACK_RECIPE_CREATE: &str = "track-recipe-create";

/// The recipe exactly as it is stored: the route's normalized title and body.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrackRecipeCreateOperationPayload {
    pub title: String,
    pub body: String,
}

#[derive(Clone, Default)]
pub struct TrackRecipeCreateAdapter;

#[async_trait]
impl TxOnlyAdapter for TrackRecipeCreateAdapter {
    fn kind(&self) -> &'static str {
        TRACK_RECIPE_CREATE
    }

    async fn validate(&self, input: &Value) -> Result<()> {
        serde_json::from_value::<TrackRecipeCreateOperationPayload>(input.clone())?;
        Ok(())
    }

    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        _op: &Operation,
    ) -> Result<TxOutput> {
        let payload: TrackRecipeCreateOperationPayload = serde_json::from_value(input.clone())?;
        let recipe = track_recipe_create_tx(tx, &payload.title, &payload.body).await?;
        Ok(TxOutput::new(
            "track_recipe",
            Some(recipe.id.clone()),
            serde_json::to_value(&recipe)?,
        ))
    }
}
