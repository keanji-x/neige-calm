//! `track-recipe-create`: the one write of `POST /api/track-recipes`. It runs through the
//! operation runtime only so the route's `Idempotency-Key` binds the recipe it made: a retry
//! under the key is answered with that recipe instead of saving a second one. The route
//! normalizes and validates the body before it submits; this inserts the row, with no side effect.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::db::sqlite::track_recipe_create_tx;
use crate::error::Result;

use super::{
    AppServerInteractOutcome, CompensationStateVersioned, CompensationStep, Operation, PhaseTag,
    ProviderAdapter, SpawnCtx, SpawnHandle, SpawnOutcome, Tx, TxOutput,
};

pub const TRACK_RECIPE_CREATE: &str = "track-recipe-create";

const TRACK_RECIPE_CREATE_PHASES: &[PhaseTag] = &[
    PhaseTag::Pending,
    PhaseTag::TxCommitted,
    PhaseTag::Succeeded,
];

/// The recipe exactly as it is stored: the route's normalized title and body.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrackRecipeCreateOperationPayload {
    pub title: String,
    pub body: String,
}

#[derive(Clone, Default)]
pub struct TrackRecipeCreateAdapter;

#[async_trait]
impl ProviderAdapter for TrackRecipeCreateAdapter {
    fn kind(&self) -> &'static str {
        TRACK_RECIPE_CREATE
    }

    fn phases(&self) -> &'static [PhaseTag] {
        TRACK_RECIPE_CREATE_PHASES
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

    async fn app_server_interact(
        &self,
        _output: &mut TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> Result<AppServerInteractOutcome> {
        Ok(AppServerInteractOutcome::NotApplicable)
    }

    async fn spawn_side_effect(
        &self,
        _output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> Result<SpawnOutcome> {
        Ok(SpawnOutcome::Ready(SpawnHandle::NoOp))
    }

    async fn plan_compensation(
        &self,
        from_phase: PhaseTag,
        reason: &str,
        _output: &TxOutput,
        _op: &Operation,
    ) -> Result<CompensationStateVersioned> {
        Ok(CompensationStateVersioned {
            version: 1,
            from_phase,
            reason: reason.to_string(),
            steps: vec![],
        })
    }

    async fn compensate_step(
        &self,
        _step: &CompensationStep,
        _output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> Result<()> {
        Ok(())
    }
}
