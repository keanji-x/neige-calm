//! Persisted native output vocabulary. No wire capabilities, IO or execution authority.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use ts_rs::TS;
use utoipa::ToSchema;

/// Optional wire fields retain JSON presence, including explicit null.
/// Kept under `fields` so wire extensions cannot collide with projection metadata.
pub type OutputFields = Map<String, Value>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum OutputBlock {
    Text {
        text: String,
        #[ts(type = "{ [key: string]: unknown }")]
        #[schema(value_type = std::collections::BTreeMap<String, serde_json::Value>)]
        fields: OutputFields,
        #[serde(default, skip_serializing_if = "Map::is_empty")]
        #[ts(optional = nullable, type = "{ [key: string]: unknown }")]
        #[schema(value_type = std::collections::BTreeMap<String, serde_json::Value>)]
        chunk: OutputFields,
    },
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
        #[ts(type = "{ [key: string]: unknown }")]
        #[schema(value_type = std::collections::BTreeMap<String, serde_json::Value>)]
        fields: OutputFields,
        #[serde(default, skip_serializing_if = "Map::is_empty")]
        #[ts(optional = nullable, type = "{ [key: string]: unknown }")]
        #[schema(value_type = std::collections::BTreeMap<String, serde_json::Value>)]
        chunk: OutputFields,
    },
    Audio {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
        #[ts(type = "{ [key: string]: unknown }")]
        #[schema(value_type = std::collections::BTreeMap<String, serde_json::Value>)]
        fields: OutputFields,
        #[serde(default, skip_serializing_if = "Map::is_empty")]
        #[ts(optional = nullable, type = "{ [key: string]: unknown }")]
        #[schema(value_type = std::collections::BTreeMap<String, serde_json::Value>)]
        chunk: OutputFields,
    },
    ResourceLink {
        uri: String,
        name: String,
        #[ts(type = "{ [key: string]: unknown }")]
        #[schema(value_type = std::collections::BTreeMap<String, serde_json::Value>)]
        fields: OutputFields,
        #[serde(default, skip_serializing_if = "Map::is_empty")]
        #[ts(optional = nullable, type = "{ [key: string]: unknown }")]
        #[schema(value_type = std::collections::BTreeMap<String, serde_json::Value>)]
        chunk: OutputFields,
    },
    Resource {
        resource: OutputResource,
        #[ts(type = "{ [key: string]: unknown }")]
        #[schema(value_type = std::collections::BTreeMap<String, serde_json::Value>)]
        fields: OutputFields,
        #[serde(default, skip_serializing_if = "Map::is_empty")]
        #[ts(optional = nullable, type = "{ [key: string]: unknown }")]
        #[schema(value_type = std::collections::BTreeMap<String, serde_json::Value>)]
        chunk: OutputFields,
    },
    /// An inert extension, never interpreted as another known block.
    UnknownExtension {
        #[ts(type = "unknown")]
        raw: Value,
        #[serde(default, skip_serializing_if = "Map::is_empty")]
        #[ts(optional = nullable, type = "{ [key: string]: unknown }")]
        #[schema(value_type = std::collections::BTreeMap<String, serde_json::Value>)]
        chunk: OutputFields,
    },
    /// Tool wrappers retain ordering and the absent/null/empty old-file distinction.
    ToolDiff {
        path: String,
        #[serde(rename = "newText")]
        new_text: String,
        #[ts(type = "{ [key: string]: unknown }")]
        #[schema(value_type = std::collections::BTreeMap<String, serde_json::Value>)]
        fields: OutputFields,
        #[serde(default, skip_serializing_if = "Map::is_empty")]
        #[ts(optional = nullable, type = "{ [key: string]: unknown }")]
        #[schema(value_type = std::collections::BTreeMap<String, serde_json::Value>)]
        chunk: OutputFields,
    },
    TerminalReference {
        #[serde(rename = "terminalId")]
        terminal_id: String,
        #[ts(type = "{ [key: string]: unknown }")]
        #[schema(value_type = std::collections::BTreeMap<String, serde_json::Value>)]
        fields: OutputFields,
        #[serde(default, skip_serializing_if = "Map::is_empty")]
        #[ts(optional = nullable, type = "{ [key: string]: unknown }")]
        #[schema(value_type = std::collections::BTreeMap<String, serde_json::Value>)]
        chunk: OutputFields,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(untagged, deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum OutputResource {
    Text {
        uri: String,
        text: String,
        #[ts(type = "{ [key: string]: unknown }")]
        #[schema(value_type = std::collections::BTreeMap<String, serde_json::Value>)]
        fields: OutputFields,
    },
    Blob {
        uri: String,
        blob: String,
        #[ts(type = "{ [key: string]: unknown }")]
        #[schema(value_type = std::collections::BTreeMap<String, serde_json::Value>)]
        fields: OutputFields,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct OutputContentV1 {
    #[ts(type = "1")]
    #[schema(value_type = u8, minimum = 1, maximum = 1)]
    pub version: OutputVersion,
    pub blocks: Vec<OutputBlock>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct OutputVersion;
impl TryFrom<u8> for OutputVersion {
    type Error = &'static str;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value == 1 {
            Ok(Self)
        } else {
            Err("unsupported output version")
        }
    }
}
impl From<OutputVersion> for u8 {
    fn from(_: OutputVersion) -> Self {
        1
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum OutputChannel {
    Message,
    Thought,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputSegment {
    pub channel: OutputChannel,
    pub ordinal: u64,
    /// Only messageId and chunkMeta; absence and null remain distinct.
    #[serde(flatten)]
    pub fields: OutputFields,
}
impl OutputContentV1 {
    pub fn new(blocks: Vec<OutputBlock>) -> Self {
        Self {
            version: OutputVersion,
            blocks,
        }
    }
    /// Complete normal and best-effort partial items use one compatibility projection.
    pub fn materialize_item(&self, mut item: Value, segment: &OutputSegment) -> Value {
        let text = self.text();
        match segment.channel {
            OutputChannel::Message => {
                item["type"] = Value::String("agentMessage".into());
                item["text"] = Value::String(text);
            }
            OutputChannel::Thought => {
                item["type"] = Value::String("reasoning".into());
                item["summary"] = serde_json::json!([text]);
                item["content"] = serde_json::json!([]);
            }
        }
        item["outputContent"] = serde_json::to_value(self).expect("output serializes");
        item["outputSegment"] = serde_json::to_value(segment).expect("segment serializes");
        item
    }
    /// Compatibility text is a projection, never an independently mutable source.
    pub fn text(&self) -> String {
        self.blocks.iter().map(OutputBlock::text).collect()
    }
}
impl OutputBlock {
    pub fn text(&self) -> &str {
        match self {
            Self::Text { text, .. } => text,
            Self::Resource {
                resource: OutputResource::Text { text, .. },
                ..
            } => text,
            _ => "",
        }
    }
}

impl OutputBlock {
    pub fn set_chunk_meta(&mut self, meta: Value) {
        self.chunk_fields().insert("meta".into(), meta);
    }
    pub fn set_tool_wrapper(&mut self, fields: Value) {
        self.chunk_fields().insert("toolWrapper".into(), fields);
    }
    fn chunk_fields(&mut self) -> &mut OutputFields {
        match self {
            Self::Text { chunk, .. }
            | Self::Image { chunk, .. }
            | Self::Audio { chunk, .. }
            | Self::ResourceLink { chunk, .. }
            | Self::Resource { chunk, .. }
            | Self::ToolDiff { chunk, .. }
            | Self::TerminalReference { chunk, .. }
            | Self::UnknownExtension { chunk, .. } => chunk,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn output_envelope_requires_version_and_blocks_without_future_fallback() {
        for bad in [
            Value::Null,
            json!({"blocks":[]}),
            json!({"version":1}),
            json!({"version":2,"blocks":[]}),
            json!({"version":1,"blocks":[{"type":"text","fields":{}}]}),
        ] {
            assert!(serde_json::from_value::<OutputContentV1>(bad).is_err());
        }
        assert_eq!(
            serde_json::to_value(OutputContentV1::new(Vec::new())).unwrap(),
            json!({"version":1,"blocks":[]})
        );
    }
}
