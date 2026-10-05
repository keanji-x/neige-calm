//! `mail ls` and `mail cat` text (#2130 §4.3, §4.4). A row is `<mail_id>  out  read  hop 1/6  <title>`,
//! with `: <summary>` in a listing; `cat` prints that header, `summary:`, the text and one last hop
//! line. `--json` is the tool result as is.

use serde_json::Value;

use super::{RenderError, compact, escape_control, required_array, required_str, shape};
use crate::mail::MAX_HOP;

/// `<mail_id>  <direction>  <state>  hop <n>/6  <title>`, every field on one line.
fn header(tool: &str, mail: &Value) -> Result<String, RenderError> {
    let [id, direction, state, hop, title] = ["mail_id", "direction", "state", "hop", "title"]
        .map(|field| required_str(mail, field, tool, "mail"));
    Ok(format!(
        "{}  {}  {}  hop {}  {}",
        escape_control(id?),
        escape_control(direction?),
        escape_control(state?),
        escape_control(hop?),
        escape_control(title?)
    ))
}

pub(super) fn ls(tool: &str, json: bool, value: &Value) -> Result<String, RenderError> {
    let mails = required_array(value, "mails", tool)?;
    if json {
        return Ok(compact(value));
    }
    let mut out = String::new();
    for mail in mails {
        let summary = required_str(mail, "summary", tool, "mail")?;
        out.push_str(&format!(
            "{}: {}\n",
            header(tool, mail)?,
            escape_control(summary)
        ));
    }
    match value.get("next_cursor") {
        Some(Value::Null) => {}
        Some(Value::String(cursor)) => out.push_str(&format!(
            "more: neige mail ls --cursor {}\n",
            escape_control(cursor)
        )),
        _ => {
            return Err(shape(
                format!("{tool} returned no string-or-null next_cursor"),
                tool,
                "value",
                value,
            ));
        }
    }
    Ok(out)
}

pub(super) fn cat(tool: &str, json: bool, value: &Value) -> Result<String, RenderError> {
    let summary = required_str(value, "summary", tool, "mail")?;
    let text = required_str(value, "text", tool, "mail")?;
    let header = header(tool, value)?;
    if json {
        return Ok(compact(value));
    }
    let mut out = format!("{header}\nsummary: {}\n\n{text}", escape_control(summary));
    if !out.ends_with('\n') {
        out.push('\n');
    }
    match value.get("next_hop") {
        Some(Value::Null) => {}
        Some(Value::String(next)) => {
            let over = next
                .split_once('/')
                .and_then(|(n, _)| n.parse::<i64>().ok())
                .is_some_and(|n| n > MAX_HOP);
            if over {
                out.push_str(&format!(
                    "hop {MAX_HOP}/{MAX_HOP} reached — hand off with neige_user_notify\n"
                ));
            } else {
                out.push_str(&format!("next hop {}\n", escape_control(next)));
            }
        }
        _ => {
            return Err(shape(
                format!("{tool} returned no string-or-null next_hop"),
                tool,
                "mail",
                value,
            ));
        }
    }
    Ok(out)
}
