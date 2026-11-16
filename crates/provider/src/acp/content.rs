//! One ACP content decoder. Output budgets are retained-state policy, not input policy.
use super::Error;
use calm_types::output_content::{OutputBlock, OutputFields};
use serde_json::{Value, json};

pub const TEXT_LIMIT: usize = 16 * 1024 * 1024;
pub const MATERIAL_LIMIT: usize = 8 * 1024 * 1024;
const METADATA_LIMIT: usize = 64 * 1024;
#[derive(Clone, Copy)]
pub enum ContentContext {
    OutputRetained,
    Compaction,
}
#[derive(Clone, Copy, Default, Debug)]
pub struct Footprint {
    pub text: usize,
    pub material: usize,
}
impl Footprint {
    pub fn combine(self, other: Self) -> Self {
        Self {
            text: self.text.saturating_add(other.text),
            material: self.material.saturating_add(other.material),
        }
    }
    pub fn replace(self, old: Self, new: Self) -> Self {
        Self {
            text: self.text - old.text,
            material: self.material - old.material,
        }
        .combine(new)
    }
    pub fn check(self) -> Result<Self, Error> {
        if self.text > TEXT_LIMIT || self.material > MATERIAL_LIMIT {
            Err(Error::Protocol("turn output exceeds retained byte limit"))
        } else {
            Ok(self)
        }
    }
}
pub fn json_size(value: &impl serde::Serialize) -> usize {
    // A counting writer avoids allocating another serialized body.
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    serde_json::to_writer(&mut count, value).expect("JSON output values serialize");
    count.0
}
pub fn bounded_metadata(value: &Value) -> Result<(), Error> {
    fn depth(v: &Value, n: usize) -> bool {
        n <= 32
            && match v {
                Value::Array(a) => a.iter().all(|v| depth(v, n + 1)),
                Value::Object(o) => o.values().all(|v| depth(v, n + 1)),
                _ => true,
            }
    }
    if !depth(value, 0) || json_size(value) > METADATA_LIMIT {
        return Err(Error::Protocol("output metadata exceeds limit"));
    }
    Ok(())
}
fn string<'a>(v: &'a Value, field: &str) -> Result<&'a str, Error> {
    v[field]
        .as_str()
        .ok_or(Error::Protocol("invalid required content string"))
}
/// Strict RFC4648 canonical alphabet/padding, checked without decoding an allocation.
fn binary(data: &str) -> Result<(), Error> {
    let bytes = data.as_bytes();
    let value = |b| match b {
        b'A'..=b'Z' => Some(b - b'A'),
        b'a'..=b'z' => Some(b - b'a' + 26),
        b'0'..=b'9' => Some(b - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    if !bytes.len().is_multiple_of(4) {
        return Err(Error::Protocol("invalid content base64"));
    }
    for (i, group) in bytes.as_chunks::<4>().0.iter().enumerate() {
        let last = (i + 1) * 4 == bytes.len();
        let valid = value(group[0]).is_some()
            && value(group[1]).is_some()
            && match (group[2], group[3]) {
                (b'=', b'=') => last && value(group[1]).is_some_and(|v| v & 15 == 0),
                (c, b'=') => last && value(c).is_some_and(|v| v & 3 == 0),
                (c, d) => value(c).is_some() && value(d).is_some(),
            };
        if !valid {
            return Err(Error::Protocol("invalid content base64"));
        }
    }
    Ok(())
}
fn optional_fields(value: &Value, strings: &[&str], resource_link: bool) -> Result<(), Error> {
    for key in ["_meta", "annotations"] {
        if let Some(v) = value.get(key)
            && !v.is_null()
            && !v.is_object()
        {
            return Err(Error::Protocol("invalid content metadata"));
        }
    }
    if let Some(annotations) = value.get("annotations").filter(|v| !v.is_null()) {
        if let Some(meta) = annotations.get("_meta")
            && !meta.is_null()
            && !meta.is_object()
        {
            return Err(Error::Protocol("invalid annotation metadata"));
        }
        if let Some(audience) = annotations.get("audience").filter(|v| !v.is_null())
            && !audience.as_array().is_some_and(|a| {
                a.iter()
                    .all(|v| matches!(v.as_str(), Some("user" | "assistant")))
            })
        {
            return Err(Error::Protocol("invalid annotation audience"));
        }
        if let Some(priority) = annotations.get("priority")
            && !priority.is_null()
            && !priority.is_number()
        {
            return Err(Error::Protocol("invalid annotation priority"));
        }
        if let Some(timestamp) = annotations.get("lastModified")
            && !timestamp.is_null()
            && !timestamp.is_string()
        {
            return Err(Error::Protocol("invalid annotation timestamp"));
        }
    }
    for key in strings {
        if let Some(v) = value.get(key)
            && !v.is_null()
            && !v.is_string()
        {
            return Err(Error::Protocol("invalid optional content string"));
        }
    }
    if let Some(v) = value.get("size").filter(|_| resource_link)
        && !v.is_null()
        && v.as_u64().is_none()
    {
        return Err(Error::Protocol("invalid resource size"));
    }
    Ok(())
}
pub fn decode(value: &Value, context: ContentContext) -> Result<OutputBlock, Error> {
    let kind = string(value, "type")?;
    let known = matches!(
        kind,
        "text" | "image" | "audio" | "resource_link" | "resource"
    );
    if !known {
        if matches!(context, ContentContext::OutputRetained) {
            bounded_metadata(value)?;
        }
        return Ok(OutputBlock::UnknownExtension {
            raw: value.clone(),
            chunk: OutputFields::new(),
        });
    }
    optional_fields(
        value,
        match kind {
            "image" => &["uri"],
            "resource_link" => &["mimeType", "title", "description"],
            _ => &[],
        },
        kind == "resource_link",
    )?;
    let mut metadata = value
        .as_object()
        .ok_or(Error::Protocol("invalid content object"))?
        .clone();
    metadata.remove("type");
    match kind {
        "text" => {
            string(value, "text")?;
            metadata.remove("text");
        }
        "image" | "audio" => {
            binary(string(value, "data")?)?;
            string(value, "mimeType")?;
            metadata.remove("data");
        }
        "resource_link" => {
            string(value, "name")?;
            string(value, "uri")?;
        }
        "resource" => {
            let resource = &value["resource"];
            string(resource, "uri")?;
            optional_fields(resource, &["mimeType"], false)?;
            if resource.get("text").is_some() == resource.get("blob").is_some() {
                return Err(Error::Protocol("resource requires text or blob"));
            }
            let mut inner = resource
                .as_object()
                .ok_or(Error::Protocol("invalid resource"))?
                .clone();
            if resource.get("text").is_some() {
                string(resource, "text")?;
                inner.remove("text");
            } else {
                binary(string(resource, "blob")?)?;
                inner.remove("blob");
            }
            metadata.insert("resource".into(), Value::Object(inner));
        }
        _ => unreachable!(),
    }
    // Independent compaction accounting stays in its existing owner. Structural validation
    // is shared, but new output metadata/retained limits do not reject its summaries.
    if matches!(context, ContentContext::OutputRetained) {
        bounded_metadata(&Value::Object(metadata))?;
    }
    let mut wire = value.as_object().expect("validated content object").clone();
    let mut projection = serde_json::Map::new();
    projection.insert("type".into(), wire.remove("type").unwrap());
    let required: &[&str] = match kind {
        "text" => &["text"],
        "image" | "audio" => &["data", "mimeType"],
        "resource_link" => &["name", "uri"],
        "resource" => &["resource"],
        _ => unreachable!(),
    };
    for key in required {
        projection.insert((*key).into(), wire.remove(*key).unwrap());
    }
    if kind == "resource" {
        let mut resource = projection["resource"].as_object().unwrap().clone();
        let mut inner = serde_json::Map::new();
        for key in [
            "uri",
            if resource.contains_key("text") {
                "text"
            } else {
                "blob"
            },
        ] {
            inner.insert(key.into(), resource.remove(key).unwrap());
        }
        inner.insert("fields".into(), Value::Object(resource));
        projection.insert("resource".into(), Value::Object(inner));
    }
    projection.insert("fields".into(), Value::Object(wire));
    serde_json::from_value(Value::Object(projection))
        .map_err(|_| Error::Protocol("invalid known content"))
}
/// Logical retained footprint: UTF-8 text bodies count once; all other compact JSON bytes
/// count as material. Compatibility/native mirrors and seal observations are not charged.
pub fn footprint(block: &OutputBlock) -> Footprint {
    let mut value = serde_json::to_value(block).expect("output block serializes");
    let text = match block {
        OutputBlock::Text { text, .. } => {
            value["text"] = json!("");
            text.len()
        }
        OutputBlock::Resource {
            resource: calm_types::output_content::OutputResource::Text { text, .. },
            ..
        } => {
            value["resource"]["text"] = json!("");
            text.len()
        }
        OutputBlock::ToolDiff {
            new_text, fields, ..
        } => {
            value["newText"] = json!("");
            let old = fields
                .get("oldText")
                .and_then(Value::as_str)
                .map_or(0, str::len);
            if old > 0 {
                value["fields"]["oldText"] = json!("");
            }
            new_text.len() + old
        }
        _ => 0,
    };
    Footprint {
        text,
        material: json_size(&value),
    }
}
pub fn tool_blocks(tool: &Value) -> Result<Vec<OutputBlock>, Error> {
    let Some(content) = tool.get("content").filter(|v| !v.is_null()) else {
        return Ok(Vec::new());
    };
    let mut result = Vec::new();
    for part in content
        .as_array()
        .ok_or(Error::Protocol("invalid tool content"))?
    {
        let block = match string(part, "type")? {
            "content" => {
                let mut block = decode(&part["content"], ContentContext::OutputRetained)?;
                let mut wrapper = part.as_object().unwrap().clone();
                wrapper.remove("type");
                wrapper.remove("content");
                if !wrapper.is_empty() {
                    bounded_metadata(&Value::Object(wrapper.clone()))?;
                    block.set_tool_wrapper(Value::Object(wrapper));
                }
                block
            }
            "diff" => {
                let path = string(part, "path")?.to_owned();
                let new_text = string(part, "newText")?.to_owned();
                let mut fields: OutputFields = part.as_object().unwrap().clone();
                for key in ["type", "path", "newText"] {
                    fields.remove(key);
                }
                if let Some(old) = fields.get("oldText")
                    && !old.is_null()
                    && !old.is_string()
                {
                    return Err(Error::Protocol("invalid diff old text"));
                }
                let mut metadata = fields.clone();
                metadata.remove("oldText");
                bounded_metadata(&json!({"path":path,"fields":metadata}))?;
                OutputBlock::ToolDiff {
                    path,
                    new_text,
                    fields,
                    chunk: OutputFields::new(),
                }
            }
            "terminal" => {
                let terminal_id = string(part, "terminalId")?.to_owned();
                bounded_metadata(part)?;
                let mut fields = part.as_object().unwrap().clone();
                fields.remove("type");
                fields.remove("terminalId");
                OutputBlock::TerminalReference {
                    terminal_id,
                    fields,
                    chunk: OutputFields::new(),
                }
            }
            _ => {
                bounded_metadata(part)?;
                OutputBlock::UnknownExtension {
                    raw: part.clone(),
                    chunk: OutputFields::new(),
                }
            }
        };
        validate_output_metadata(&block)?;
        result.push(block);
    }
    Ok(result)
}

/// Legacy tool consumers receive a projection of the same decoded blocks.
pub fn tool_compatibility(block: &OutputBlock) -> Value {
    match block {
        OutputBlock::ToolDiff {
            path,
            new_text,
            fields,
            ..
        } => {
            json!({"type":"text","text":format!("{path}\n---\n{}\n+++\n{new_text}",fields.get("oldText").and_then(Value::as_str).unwrap_or(""))})
        }
        OutputBlock::TerminalReference { terminal_id, .. } => {
            json!({"type":"text","text":format!("Agent terminal: {terminal_id}")})
        }
        OutputBlock::UnknownExtension { .. } => {
            json!({"type":"text","text":"The agent returned an ACP content extension; its native record is retained."})
        }
        _ => wire_block(block),
    }
}
/// Rebuild a validated wire block from its typed projection, preserving every optional key.
pub fn wire_block(block: &OutputBlock) -> Value {
    let mut value = serde_json::to_value(block).expect("output serializes");
    let object = value.as_object_mut().unwrap();
    object.remove("chunk");
    if let Some(Value::Object(fields)) = object.remove("fields") {
        object.extend(fields);
    }
    if let Some(resource) = object.get_mut("resource").and_then(Value::as_object_mut)
        && let Some(Value::Object(fields)) = resource.remove("fields")
    {
        resource.extend(fields);
    }
    value
}

/// Validate the combined retained metadata after attaching chunk or tool-wrapper metadata.
/// Body bytes and binary data have separate output budgets.
pub fn validate_output_metadata(block: &OutputBlock) -> Result<(), Error> {
    let mut metadata = serde_json::to_value(block).expect("output serializes");
    match block {
        OutputBlock::Text { .. } => {
            metadata.as_object_mut().unwrap().remove("text");
        }
        OutputBlock::Image { .. } | OutputBlock::Audio { .. } => {
            metadata.as_object_mut().unwrap().remove("data");
        }
        OutputBlock::Resource { .. } => {
            let r = metadata["resource"].as_object_mut().unwrap();
            r.remove("text");
            r.remove("blob");
        }
        OutputBlock::ToolDiff { .. } => {
            metadata.as_object_mut().unwrap().remove("newText");
            metadata["fields"]
                .as_object_mut()
                .unwrap()
                .remove("oldText");
        }
        _ => {}
    }
    bounded_metadata(&metadata)
}
