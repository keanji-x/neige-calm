//! `mail ls` and `mail cat` text (#2130 §4.3, §4.4). A row is `<mail_id>  out  read  hop 1/6  <title>`,
//! with `: <summary>` in a listing; `cat` prints that header, `summary:`, the text and one last hop
//! line. `--json` is the tool result as is.

use serde_json::Value;

use super::{RenderError, compact, escape_control, required_array, required_str, shape};
use crate::mail::MAX_HOP;

/// `<mail_id>  <direction>  <state>  hop <n>/6  <title>`, every field on one line; an untitled
/// Track is named by its track id.
fn header(tool: &str, mail: &Value) -> Result<String, RenderError> {
    let [id, direction, state, hop, title, track] =
        ["mail_id", "direction", "state", "hop", "title", "track_id"]
            .map(|field| required_str(mail, field, tool, "mail"));
    let title = title?;
    let title = if title.trim().is_empty() {
        track
    } else {
        Ok(title)
    };
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
    let refused = value
        .get("refused")
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            shape(
                format!("{tool} returned no boolean refused"),
                tool,
                "mail",
                value,
            )
        })?;
    match (refused, value.get("next_hop")) {
        (true, Some(Value::Null)) => out.push_str(&format!(
            "hop {MAX_HOP}/{MAX_HOP} reached — hand off with neige_user_ask\n"
        )),
        (false, Some(Value::Null)) => {}
        (false, Some(Value::String(next))) => {
            out.push_str(&format!("next hop {}\n", escape_control(next)));
        }
        _ => {
            return Err(shape(
                format!("{tool} returned invalid refused/next_hop"),
                tool,
                "mail",
                value,
            ));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mail_cat_renders_structured_hop_refusal() {
        let mut mail = json!({
            "mail_id": "m", "direction": "in", "state": "read", "hop": "6/6",
            "title": "Peer", "track_id": "tr", "summary": "s", "text": "body",
            "next_hop": null, "refused": true
        });
        let rendered = cat("neige_mail_cat", false, &mail).unwrap();
        assert!(rendered.ends_with("hop 6/6 reached — hand off with neige_user_ask\n"));
        assert_eq!(cat("neige_mail_cat", true, &mail).unwrap(), compact(&mail));
        mail["refused"] = json!(false);
        assert!(
            cat("neige_mail_cat", false, &mail)
                .unwrap()
                .ends_with("body\n")
        );
        mail["next_hop"] = json!("6/6");
        assert!(
            cat("neige_mail_cat", false, &mail)
                .unwrap()
                .ends_with("next hop 6/6\n")
        );
        for bad in [Value::Null, json!("true"), json!(1)] {
            mail["refused"] = bad;
            assert!(cat("neige_mail_cat", false, &mail).is_err());
        }
        mail.as_object_mut().unwrap().remove("refused");
        assert!(cat("neige_mail_cat", false, &mail).is_err());
    }
}
