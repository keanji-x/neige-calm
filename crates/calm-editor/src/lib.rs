//! Editor wire types exported to the web client.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[ts(export, export_to = "web/src/editor/types/")]
pub struct EditorDoc {
    #[ts(type = "unknown")]
    pub value: serde_json::Value,
}
