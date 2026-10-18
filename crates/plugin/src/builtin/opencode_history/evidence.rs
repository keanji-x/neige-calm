//! Retain complete evidence units; budget the actual serialized MCP success result.
use super::{Result, config::Limits};
use crate::mcp::CallToolResult;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
fn size(v: &Value) -> usize {
    serde_json::to_vec(v).expect("JSON").len()
}
fn add(v: &mut Value, key: &str, n: usize) {
    v[key] = json!(v[key].as_u64().unwrap_or(0) + n as u64);
}
fn omit(msg: &mut Value, part: Value, reason: &str, coverage: &mut Value) {
    let n = size(&part);
    msg["omissions"]
        .as_array_mut()
        .expect("omissions")
        .push(json!({"id":part["id"],"type":part["type"],"bytes":n,"reason":reason}));
    add(coverage, "omitted_parts", 1);
    add(coverage, "omitted_part_bytes", n);
}
pub fn bounded(mut e: Value, l: &Limits) -> Result<CallToolResult> {
    let mut coverage = e["coverage"].take();
    let mut retained = 0;
    if let Some(messages) = e["messages"].as_array_mut() {
        for msg in messages {
            msg["omissions"] = json!([]);
            let parts = msg["parts"]
                .take()
                .as_array()
                .expect("validated parts")
                .clone();
            let mut keep = Vec::new();
            for part in parts {
                if retained >= l.max_parts || size(&part) > l.max_part_bytes {
                    omit(msg, part, "part_budget", &mut coverage);
                } else {
                    keep.push(part);
                    retained += 1;
                }
            }
            msg["parts"] = json!(keep);
        }
    }
    coverage["retained_parts"] = json!(retained);
    e["coverage"] = coverage;
    loop {
        e.as_object_mut().expect("envelope").remove("digest");
        let digest = hex::encode(Sha256::digest(serde_json::to_vec(&e).expect("JSON")));
        e["digest"] =
            json!({"algorithm":"sha256","scope":"retained-envelope-without-digest","value":digest});
        let result: CallToolResult = serde_json::from_value(
            json!({"content":[{"type":"text","text":e.to_string()}],"isError":false}),
        )
        .expect("MCP result");
        if serde_json::to_vec(&result).expect("result").len() <= l.max_result_bytes {
            return Ok(result);
        }
        let mut c = e["coverage"].take();
        let mut removed = false;
        if let Some(messages) = e["messages"].as_array_mut() {
            for msg in messages.iter_mut().rev() {
                if let Some(part) = msg["parts"].as_array_mut().expect("parts").pop() {
                    omit(msg, part, "result_budget", &mut c);
                    c["retained_parts"] = json!(c["retained_parts"].as_u64().expect("count") - 1);
                    removed = true;
                    break;
                }
            }
            if !removed && let Some(msg) = messages.pop() {
                e["omitted_messages"]
                    .as_array_mut()
                    .expect("omitted messages")
                    .push(json!({"id":msg["info"]["id"],"reason":"result_budget"}));
                add(&mut c, "omitted_messages", 1);
                removed = true;
            }
        }
        if !removed
            && let Some(items) = e["items"].as_array_mut()
            && items.pop().is_some()
        {
            add(&mut c, "omitted_items", 1);
            removed = true;
        }
        e["coverage"] = c;
        if !removed {
            return Err("budget: truthful envelope cannot fit; no capture claim".into());
        }
    }
}
