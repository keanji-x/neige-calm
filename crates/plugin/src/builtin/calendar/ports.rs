//! Calendar storage port: transaction execution and event persistence are kernel-owned.
use crate::ports::ErrorFactory;
use async_trait::async_trait;
use calm_types::{
    event::{Event, EventScope},
    ids::ActorId,
    model::Track,
};
use serde_json::Value;
use std::{future::Future, pin::Pin};
pub type TxFuture<'a, E> =
    Pin<Box<dyn Future<Output = Result<(Value, Vec<(EventScope, Event)>), E>> + Send + 'a>>;
pub type Mutation<E> =
    Box<dyn for<'a> FnOnce(&'a mut dyn Transaction<E>) -> TxFuture<'a, E> + Send>;
pub enum CommitMode {
    Events(ActorId),
    Silent,
}
#[async_trait]
pub trait Transaction<E: ErrorFactory>: Send {
    fn new_id(&self) -> String;
    fn now_ms(&self) -> i64;
    async fn read(&mut self, key: &str) -> Result<Option<String>, E>;
    async fn put(&mut self, key: &str, value: &str) -> Result<(), E>;
    async fn track_is_open(&mut self, track: &str) -> Result<bool, E>;
}
#[async_trait]
pub trait Storage: Send + Sync {
    type Error: ErrorFactory;
    async fn list(&self, prefix: &str) -> Result<Vec<(String, Value)>, Self::Error>;
    async fn get(&self, key: &str) -> Result<Option<Value>, Self::Error>;
    async fn track(&self, id: &str) -> Result<Option<Track>, Self::Error>;
    async fn is_running(&self) -> bool;
    async fn commit(
        &self,
        mode: CommitMode,
        mutation: Mutation<Self::Error>,
    ) -> Result<Value, Self::Error>;
}
