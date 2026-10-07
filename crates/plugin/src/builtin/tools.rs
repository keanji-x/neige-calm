//! A native tool's declared contract, independent of the kernel registry.
use calm_types::model::CardRole;
use serde_json::Value;
#[derive(Clone)]
pub struct NativeToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub annotations: Option<Value>,
    pub roles: &'static [CardRole],
    pub listed_for: &'static [CardRole],
}
