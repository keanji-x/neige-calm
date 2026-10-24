//! xAI's manual maintenance extension. It is distinct from standard ACP compaction.
use super::Error;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const METHOD: &str = "_x.ai/compact_conversation";

#[derive(Serialize)]
struct Request<'a> {
    session_id: &'a str,
    user_context: &'a str,
}
pub fn encode(session_id: &str, user_context: &str) -> Value {
    serde_json::to_value(Request {
        session_id,
        user_context,
    })
    .expect("string request")
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Completion {
    pub session_id: String,
    update: Update,
    #[serde(rename = "_meta")]
    meta: Option<Value>,
}
#[derive(Deserialize)]
struct Update {
    tokens_before: u64,
    tokens_after: u64,
}
impl Completion {
    pub fn is_replay(&self) -> Result<bool, Error> {
        match self.meta.as_ref().and_then(|meta| meta.get("isReplay")) {
            None => Ok(false),
            Some(Value::Bool(replay)) => Ok(*replay),
            Some(_) => Err(Error::Protocol("invalid xAI replay marker")),
        }
    }
}
/// Replay's `_x.ai/session/update` is deliberately not a live completion method.
pub fn decode(method: &str, params: &Value) -> Result<Option<Completion>, Error> {
    if method != "_x.ai/session_notification"
        || params["update"]["sessionUpdate"] != "auto_compact_completed"
    {
        return Ok(None);
    }
    let completion: Completion = serde_json::from_value(params.clone())
        .map_err(|_| Error::Protocol("invalid xAI completion"))?;
    if completion.session_id.is_empty() {
        return Err(Error::Protocol("empty xAI session id"));
    }
    // Equal token counts are valid. These fields describe the event, not success policy.
    let _ = (
        completion.update.tokens_before,
        completion.update.tokens_after,
    );
    if completion
        .meta
        .as_ref()
        .is_some_and(|meta| !meta.is_object())
    {
        return Err(Error::Protocol("invalid xAI metadata"));
    }
    completion.is_replay()?;
    Ok(Some(completion))
}
pub fn empty_response(value: &Value) -> bool {
    value.as_object().is_some_and(|object| object.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn completed() -> Value {
        json!({"sessionId":"bound", "update":{"sessionUpdate":"auto_compact_completed",
            "tokens_before":23450,"tokens_after":23450,"summary_preview":null},
            "_meta":{"eventId":"opaque", "agentTimestampMs":1792827612149_i64}})
    }
    #[test]
    fn acp_grok_compact_wire_shape_and_required_fields() {
        assert_eq!(
            encode("bound", "keep facts"),
            json!({"session_id":"bound", "user_context":"keep facts"})
        );
        let p = completed();
        assert_eq!(
            decode("_x.ai/session_notification", &p)
                .unwrap()
                .unwrap()
                .session_id,
            "bound"
        );
        let mut missing = p.clone();
        missing.as_object_mut().unwrap().remove("sessionId");
        assert!(decode("_x.ai/session_notification", &missing).is_err());
        let mut invalid = p.clone();
        invalid["session_id"] = invalid["sessionId"].take();
        assert!(decode("_x.ai/session_notification", &invalid).is_err());
        for key in ["tokens_before", "tokens_after"] {
            let mut missing = p.clone();
            missing["update"].as_object_mut().unwrap().remove(key);
            assert!(decode("_x.ai/session_notification", &missing).is_err());
        }
        let mut unequal = p.clone();
        unequal["update"]["tokens_after"] = json!(100);
        assert!(
            decode("_x.ai/session_notification", &unequal)
                .unwrap()
                .is_some()
        );
        for tag in [
            "compaction_checkpoint",
            "session_summary_generated",
            "compaction_update",
        ] {
            let mut other = p.clone();
            other["update"]["sessionUpdate"] = json!(tag);
            assert!(
                decode("_x.ai/session_notification", &other)
                    .unwrap()
                    .is_none()
            );
        }
    }
    #[test]
    fn acp_grok_compact_replay_metadata_location() {
        let mut p = completed();
        p["_meta"]["isReplay"] = json!(true);
        assert!(
            decode("_x.ai/session_notification", &p)
                .unwrap()
                .unwrap()
                .is_replay()
                .unwrap()
        );
        assert!(decode("_x.ai/session/update", &p).unwrap().is_none());
        p["_meta"]["isReplay"] = json!("true");
        assert!(decode("_x.ai/session_notification", &p).is_err());
        p.as_object_mut().unwrap().remove("_meta");
        p["update"]["_meta"] = json!({"isReplay":true});
        assert!(
            !decode("_x.ai/session_notification", &p)
                .unwrap()
                .unwrap()
                .is_replay()
                .unwrap()
        );
    }
    #[test]
    fn acp_grok_compact_empty_response_required() {
        assert!(empty_response(&json!({})));
        for invalid in [
            json!(null),
            json!([]),
            json!({"stopReason":"end_turn"}),
            json!({"ok":true}),
        ] {
            assert!(!empty_response(&invalid));
        }
    }
}
