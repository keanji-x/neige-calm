//! Bounded, resumable text selections after the owning reader has authorized a virtual path.
use super::super::framing::RpcError;
use serde::Deserialize;
use serde_json::{Value, json};

const PAGE_BYTES: usize = 8192;

#[derive(Default, Deserialize)]
pub(super) struct OutputSelect {
    start_line: Option<usize>,
    end_line: Option<usize>,
    tail: Option<usize>,
    step: Option<String>,
    grep: Option<String>,
    byte_offset: Option<usize>,
    pub rev: Option<Value>,
}

impl OutputSelect {
    pub fn schema() -> Value {
        json!({
            "start_line":{"type":"integer","minimum":1},
            "end_line":{"type":"integer","minimum":1},
            "tail":{"type":"integer","minimum":1},
            "step":{"type":"string","minLength":1},
            "grep":{"type":"string"},
            "byte_offset":{"type":"integer","minimum":0},
            "rev":{"type":["integer","string"]}
        })
    }

    pub fn parse(args: &Value) -> Result<Self, RpcError> {
        let selected: serde_json::Map<_, _> = args
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(key, _)| Self::schema().get(key.as_str()).is_some())
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        if selected.values().any(Value::is_null) {
            return Err(RpcError::invalid_params("output selectors cannot be null"));
        }
        let this: Self = serde_json::from_value(Value::Object(selected))
            .map_err(|_| RpcError::invalid_params("invalid output selectors"))?;
        if this.start_line == Some(0)
            || this.end_line == Some(0)
            || this.tail == Some(0)
            || this.step.as_ref().is_some_and(|step| step.is_empty())
            || this
                .start_line
                .zip(this.end_line)
                .is_some_and(|(a, b)| a > b)
            || (this.tail.is_some() && (this.start_line.is_some() || this.end_line.is_some()))
        {
            return Err(RpcError::invalid_params(
                "invalid output range or tail/range combination",
            ));
        }
        if this.byte_offset.is_some_and(|offset| offset > 0) && this.rev.is_none() {
            return Err(RpcError::invalid_params(
                "continuation byte_offset requires rev",
            ));
        }
        Ok(this)
    }

    pub fn step_is_set(&self) -> bool {
        self.step.is_some()
    }

    pub fn active(&self) -> bool {
        self.start_line.is_some()
            || self.end_line.is_some()
            || self.tail.is_some()
            || self.step.is_some()
            || self.grep.is_some()
            || self.byte_offset.is_some()
    }

    pub fn check_rev(&self, rev: &Value) -> Result<(), RpcError> {
        if self.rev.as_ref().is_some_and(|expected| expected != rev) {
            return Err(RpcError::invalid_params(
                "output revision changed; read again without rev",
            ));
        }
        Ok(())
    }

    pub fn project(&self, path: &str, text: &str, rev: Value) -> Result<Value, RpcError> {
        self.check_rev(&rev)?;
        let mut current_step = None;
        let mut rows = Vec::new();
        for (index, line) in text.split_inclusive('\n').enumerate() {
            if let Some(step) = line
                .trim_end_matches(['\r', '\n'])
                .strip_prefix("::gate-step ")
            {
                current_step = Some(step);
            }
            if self
                .step
                .as_deref()
                .is_none_or(|step| current_step == Some(step))
                && self.grep.as_ref().is_none_or(|grep| line.contains(grep))
                && self.start_line.is_none_or(|start| index + 1 >= start)
                && self.end_line.is_none_or(|end| index + 1 <= end)
            {
                rows.push((index + 1, line));
            }
        }
        if let Some(tail) = self.tail {
            rows = rows.split_off(rows.len().saturating_sub(tail));
        }
        let total_bytes: usize = rows.iter().map(|(_, line)| line.len()).sum();
        let offset = self.byte_offset.unwrap_or(0);
        if offset > total_bytes {
            return Err(RpcError::invalid_params(
                "byte_offset exceeds selected text",
            ));
        }
        let mut skip = offset;
        let mut budget = PAGE_BYTES;
        let mut consumed = 0;
        let mut lines = Vec::new();
        for (number, line) in rows {
            if skip >= line.len() {
                skip -= line.len();
                continue;
            }
            if !line.is_char_boundary(skip) {
                return Err(RpcError::invalid_params(
                    "byte_offset must be a UTF-8 boundary",
                ));
            }
            let start = skip;
            skip = 0;
            let mut end = (start + budget).min(line.len());
            while !line.is_char_boundary(end) {
                end -= 1;
            }
            if end == start {
                break;
            }
            lines.push(
                json!({"line":number,"byte_start":start,"byte_end":end,"text":&line[start..end]}),
            );
            consumed += end - start;
            budget -= end - start;
            if budget == 0 || end < line.len() {
                break;
            }
        }
        let next = offset + consumed;
        let mut projected = json!({"path":path,"rev":rev,"step":self.step,"grep":self.grep,
            "start_line":self.start_line,"end_line":self.end_line,"tail":self.tail,
            "byte_offset":offset,"lines":lines,"content_type":"text/plain",
            "next_byte_offset":(next<total_bytes).then_some(next),
            "total_selected_bytes":total_bytes});
        projected["content"] = Value::String(calm_types::observation::receipt::escape_json(
            &projected.to_string(),
        ));
        Ok(projected)
    }
}
