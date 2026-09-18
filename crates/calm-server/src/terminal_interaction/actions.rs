//! Input action validation and encoding against the live input surface.
//! Every action is one ordered write request: one barrier, one
//! acknowledgement, one receipt (no claim about OS-level write atomicity).
use super::replace_plan::REPLACE_FROM_BYTES_MAX;
use anyhow::{Result, ensure};
use calm_terminal_view::{InputSurface, click_bytes, key_bytes};
use serde_json::Value;

/// Bound on the bytes one action may write (text and sequences alike).
pub const ACTION_BYTES_MAX: usize = 16384;
/// A sequence (#1666) carries 2..=8 steps.
pub const SEQUENCE_STEPS_MIN: usize = 2;
pub const SEQUENCE_STEPS_MAX: usize = 8;
/// Keys a sequence step may send: cursor movement and draft editing only, so
/// a sequence can never carry a CR or an LF. What Up/Down/Home/End/Ctrl+U
/// do is application-defined (history recall, line edit, or something else).
pub const SEQUENCE_KEYS: [&str; 9] = [
    "Left",
    "Right",
    "Up",
    "Down",
    "Home",
    "End",
    "Backspace",
    "Delete",
    "Ctrl+U",
];

/// What one action writes: its bytes, a `submit` (#1725: the text plus one
/// CR, which the writer hands to the PTY as two physical writes), or (#1677)
/// a `replace` whose bytes are derived from the live cursor row at the
/// pre-write fences.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Encoded {
    Bytes(Vec<u8>),
    Submit(Vec<u8>),
    Replace { from: String, to: String },
}
impl Encoded {
    /// The bytes of an action that carries them (tests).
    #[cfg(test)]
    fn bytes(self) -> Vec<u8> {
        match self {
            Self::Bytes(bytes) | Self::Submit(bytes) => bytes,
            Self::Replace { .. } => panic!("replace carries no bytes before the plan"),
        }
    }
}
/// A `replace` action (#1677): `from` nonempty printable text of at most
/// [`REPLACE_FROM_BYTES_MAX`] bytes, `to` printable text (may be empty),
/// neither with control characters (so no CR or LF), no other fields. The
/// plan is derived from the live frame later; this is the shape check that
/// runs before any claim.
fn replace_arguments(action: &Value, object: &serde_json::Map<String, Value>) -> Result<Encoded> {
    ensure!(
        object.len() == 3 && object.contains_key("from") && object.contains_key("to"),
        "replace action accepts only type/from/to"
    );
    let field = |name: &str| {
        action[name]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("replace {name} must be a string"))
    };
    let (from, to) = (field("from")?, field("to")?);
    ensure!(
        !from.is_empty()
            && from.len() <= REPLACE_FROM_BYTES_MAX
            && !from.chars().any(char::is_control),
        "replace from must be 1..{REPLACE_FROM_BYTES_MAX} bytes of printable text"
    );
    ensure!(
        to.len() <= ACTION_BYTES_MAX && !to.chars().any(char::is_control),
        "replace to must be printable text (empty deletes); send Enter as its own action"
    );
    Ok(Encoded::Replace {
        from: from.to_owned(),
        to: to.to_owned(),
    })
}
/// The `text` field of a text-like action: nonempty, at most 16384 bytes, no
/// control characters, and no other fields on the action.
fn printable_text<'a>(
    action: &'a Value,
    object: &serde_json::Map<String, Value>,
    kind: &str,
) -> Result<&'a str> {
    ensure!(
        object.len() == 2 && object.contains_key("text"),
        "{kind} action accepts only type/text"
    );
    let text = action["text"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("text required"))?;
    ensure!(
        text.len() <= ACTION_BYTES_MAX && !text.is_empty() && !text.chars().any(char::is_control),
        "text must be nonempty printable text; use explicit keys for Enter or controls"
    );
    Ok(text)
}
/// A `key` action: `key` with an optional bounded `repeat`. `allowed`
/// restricts the vocabulary (sequence steps); `None` accepts every key
/// [`key_bytes`] knows.
fn encode_key(
    action: &Value,
    object: &serde_json::Map<String, Value>,
    surface: &InputSurface,
    allowed: Option<&[&str]>,
) -> Result<Vec<u8>> {
    ensure!(
        (2..=3).contains(&object.len())
            && object
                .keys()
                .all(|field| matches!(field.as_str(), "type" | "key" | "repeat")),
        "key action accepts only type/key/repeat"
    );
    let key = action["key"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("key required"))?;
    if let Some(allowed) = allowed {
        ensure!(
            allowed.contains(&key),
            "a sequence step may send only {allowed:?}; send Enter, Escape, Tab or control keys as their own action"
        );
    }
    let repeat = match object.get("repeat") {
        None => 1,
        Some(value) => value
            .as_u64()
            .filter(|value| (1..=32).contains(value))
            .ok_or_else(|| anyhow::anyhow!("repeat must be an integer from 1 to 32"))?,
    };
    ensure!(
        repeat == 1
            || matches!(
                key,
                "Left" | "Right" | "Up" | "Down" | "Backspace" | "Delete"
            ),
        "only navigation and editing keys may repeat"
    );
    // One bounded action, one receipt and one physical ownership barrier.
    // Never turn Enter, Escape or control keys into repeated submissions.
    Ok(key_bytes(key, surface.modes)?.repeat(repeat as usize))
}
/// One step of a sequence (#1666): a `text` or a `key` action with the
/// sequence key vocabulary; nothing else (no submit, click or nesting).
fn encode_step(step: &Value, surface: &InputSurface) -> Result<Vec<u8>> {
    let object = step
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("sequence step must be an object"))?;
    match step["type"].as_str() {
        Some("text") => Ok(printable_text(step, object, "text")?.as_bytes().to_vec()),
        Some("key") => encode_key(step, object, surface, Some(&SEQUENCE_KEYS)),
        _ => anyhow::bail!("sequence steps must be text or key actions"),
    }
}
/// Number of steps when `action` is a sequence (validated elsewhere).
pub fn sequence_steps(action: &Value) -> Option<usize> {
    (action["type"].as_str() == Some("sequence"))
        .then(|| action["steps"].as_array().map(Vec::len))
        .flatten()
}
/// The actions `allow_output_below_cursor` may admit (#1666 r1): draft
/// edits only — `text`, `sequence`, `replace` (#1677) and a `key` from
/// [`SEQUENCE_KEYS`].
/// Claude Code's slash-command menu renders below the input row and
/// re-sorts while it loads, so an Enter admitted by the tolerance could pick
/// a different item than the one observed; a submission in a field whose
/// status text moves keeps using `allow_output_since_observation` after
/// inspecting the fresh state. Never submit, click, Enter, Tab, Escape,
/// other control keys or PageUp/PageDown.
pub fn edits_the_draft(action: &Value) -> bool {
    match action["type"].as_str() {
        Some("text" | "sequence" | "replace") => true,
        Some("key") => action["key"]
            .as_str()
            .is_some_and(|key| SEQUENCE_KEYS.contains(&key)),
        _ => false,
    }
}
/// Reason `allow_output_below_cursor` is refused for other actions.
pub const BELOW_CURSOR_EDITS_ONLY: &str = "allow_output_below_cursor admits only text, sequence, replace and editing keys; no submit/click/Enter/Tab/Escape/Ctrl";
pub fn encode(action: &Value, surface: &InputSurface) -> Result<Encoded> {
    let object = action
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("terminal action must be an object"))?;
    let bytes = match action["type"].as_str() {
        Some("replace") => return replace_arguments(action, object),
        Some("text") => Ok(printable_text(action, object, "text")?.as_bytes().to_vec()),
        Some("submit") => {
            // #1620/#1725 — one request, one receipt, one barrier; the
            // writer hands the CR to the PTY as a second write after the
            // text. Explicit opt-in; `text` alone never submits and
            // `submit` never repeats.
            let mut bytes = printable_text(action, object, "submit")?
                .as_bytes()
                .to_vec();
            bytes.push(b'\r');
            return Ok(Encoded::Submit(bytes));
        }
        Some("key") => encode_key(action, object, surface, None),
        Some("click") => {
            ensure!(object.len() == 3, "click accepts only type/column/row");
            let coordinate = |field: &str| {
                action[field]
                    .as_u64()
                    .and_then(|value| u16::try_from(value).ok())
                    .ok_or_else(|| anyhow::anyhow!("invalid cell coordinate"))
            };
            click_bytes(coordinate("column")?, coordinate("row")?, surface)
        }
        Some("sequence") => {
            // #1666 — a bounded edit in one ordered write request: the step
            // encodings concatenated, one barrier, one acknowledgement, one
            // receipt, one fingerprint (no claim about OS-level write or
            // read atomicity). The tool guarantees no CR and no LF (the key
            // vocabulary has neither); it does not guarantee what the
            // application does with Up/Down/Home/End/Ctrl+U.
            ensure!(
                object.len() == 2 && object.contains_key("steps"),
                "sequence action accepts only type/steps"
            );
            let steps = action["steps"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("steps must be an array"))?;
            ensure!(
                (SEQUENCE_STEPS_MIN..=SEQUENCE_STEPS_MAX).contains(&steps.len()),
                "sequence must carry {SEQUENCE_STEPS_MIN}..{SEQUENCE_STEPS_MAX} steps"
            );
            let mut bytes = Vec::new();
            for step in steps {
                bytes.extend(encode_step(step, surface)?);
                ensure!(
                    bytes.len() <= ACTION_BYTES_MAX,
                    "sequence exceeds {ACTION_BYTES_MAX} encoded bytes"
                );
            }
            Ok(bytes)
        }
        _ => anyhow::bail!("unknown terminal action"),
    };
    bytes.map(Encoded::Bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn surface() -> InputSurface {
        calm_terminal_view::TerminalView::new(80, 24, [220; 3], [20; 3])
            .unwrap()
            .frame(0)
            .unwrap()
            .input_surface()
    }

    /// #1620 `submit`: the text bytes plus exactly one CR in one request
    /// (#1725: two PTY writes, see `only_submit_is_encoded_as_a_submit`);
    /// the same text rules as `text`; no repeat, no extra fields.
    #[test]
    fn submit_encodes_text_and_one_cr_and_rejects_repeat() {
        let surface = surface();
        assert_eq!(
            encode(&json!({"type":"submit","text":"ls -la"}), &surface)
                .unwrap()
                .bytes(),
            b"ls -la\r".to_vec()
        );
        assert_eq!(
            encode(&json!({"type":"text","text":"ls -la"}), &surface)
                .unwrap()
                .bytes(),
            b"ls -la".to_vec(),
            "text alone never submits"
        );
        for invalid in [
            json!({"type":"submit","text":"x","repeat":2}),
            json!({"type":"submit","text":""}),
            json!({"type":"submit","text":"a\nb"}),
            json!({"type":"submit"}),
            json!({"type":"submit","text":"x".repeat(16385)}),
        ] {
            assert!(encode(&invalid, &surface).is_err(), "{invalid}");
        }
    }

    /// #1725 — only `submit` is marked for the split write: the encoder
    /// returns `Encoded::Submit` for it and plain `Encoded::Bytes` for an
    /// Enter key (exactly one CR), text, a sequence and every other key;
    /// a replace stays `Encoded::Replace`.
    #[test]
    fn only_submit_is_encoded_as_a_submit() {
        let surface = surface();
        assert_eq!(
            encode(&json!({"type":"submit","text":"hello"}), &surface).unwrap(),
            Encoded::Submit(b"hello\r".to_vec())
        );
        assert_eq!(
            encode(&json!({"type":"key","key":"Enter"}), &surface).unwrap(),
            Encoded::Bytes(b"\r".to_vec()),
            "Enter is one CR, written verbatim"
        );
        for action in [
            json!({"type":"text","text":"hello"}),
            json!({"type":"sequence","steps":[{"type":"text","text":"a"},{"type":"key","key":"Left"}]}),
            json!({"type":"key","key":"Ctrl+J"}),
            json!({"type":"key","key":"Left","repeat":3}),
            json!({"type":"key","key":"Tab"}),
        ] {
            let encoded = encode(&action, &surface).unwrap();
            assert!(
                matches!(encoded, Encoded::Bytes(_)),
                "{action}: {encoded:?}"
            );
        }
        assert!(matches!(
            encode(&json!({"type":"replace","from":"a","to":"b"}), &surface).unwrap(),
            Encoded::Replace { .. }
        ));
    }

    /// #1666 `sequence`: the concatenation of its steps in order, the same
    /// text and repeat rules per step, only the editing key vocabulary, and
    /// no submit, click, nesting, or size past the action bound.
    #[test]
    fn sequence_encodes_steps_in_order_and_rejects_submission_keys() {
        let surface = surface();
        let edit = json!({"type":"sequence","steps":[
            {"type":"text","text":"7200 + 19"},
            {"type":"key","key":"Left","repeat":5},
            {"type":"key","key":"Backspace"},
            {"type":"text","text":"9"}]});
        assert_eq!(
            encode(&edit, &surface).unwrap().bytes(),
            b"7200 + 19\x1b[D\x1b[D\x1b[D\x1b[D\x1b[D\x7f9".to_vec()
        );
        assert_eq!(sequence_steps(&edit), Some(4));
        assert_eq!(sequence_steps(&json!({"type":"text","text":"x"})), None);
        let clear = json!({"type":"sequence","steps":[{"type":"key","key":"Ctrl+U"},{"type":"text","text":"new draft"}]});
        assert_eq!(
            encode(&clear, &surface).unwrap().bytes(),
            b"\x15new draft".to_vec()
        );
        let home = json!({"type":"sequence","steps":[{"type":"text","text":"world"},{"type":"key","key":"Home"},{"type":"text","text":"hello "}]});
        assert_eq!(
            encode(&home, &surface).unwrap().bytes(),
            b"world\x1b[Hhello ".to_vec()
        );
        for key in SEQUENCE_KEYS {
            let steps = json!({"type":"sequence","steps":[{"type":"key","key":key},{"type":"text","text":"x"}]});
            let bytes = encode(&steps, &surface)
                .unwrap_or_else(|e| panic!("{key}: {e}"))
                .bytes();
            assert!(
                !bytes.contains(&b'\r') && !bytes.contains(&b'\n'),
                "{key}: a sequence never carries CR or LF"
            );
        }
        let steps = |list: Value| json!({"type":"sequence","steps":list});
        let text = json!({"type":"text","text":"x"});
        for (invalid, why) in [
            (steps(json!([text, {"type":"key","key":"Enter"}])), "Enter"),
            (
                steps(json!([text, {"type":"key","key":"Ctrl+J"}])),
                "Ctrl+J",
            ),
            (
                steps(json!([text, {"type":"key","key":"Escape"}])),
                "Escape",
            ),
            (steps(json!([text, {"type":"key","key":"Tab"}])), "Tab"),
            (
                steps(json!([text, {"type":"key","key":"Ctrl+C"}])),
                "Ctrl+C",
            ),
            (
                steps(json!([text, {"type":"key","key":"Ctrl+D"}])),
                "Ctrl+D",
            ),
            (
                steps(json!([text, {"type":"key","key":"Ctrl+L"}])),
                "Ctrl+L",
            ),
            (
                steps(json!([text, {"type":"key","key":"PageUp"}])),
                "PageUp",
            ),
            (
                steps(json!([text, {"type":"key","key":"PageDown"}])),
                "PageDown",
            ),
            (steps(json!([text, {"type":"submit","text":"x"}])), "submit"),
            (
                steps(json!([text, {"type":"click","column":0,"row":0}])),
                "click",
            ),
            (steps(json!([text, steps(json!([text, text]))])), "nesting"),
            (steps(json!([text])), "one step"),
            (
                steps(json!([
                    text, text, text, text, text, text, text, text, text
                ])),
                "nine steps",
            ),
            (steps(json!([])), "no steps"),
            (
                steps(json!([text, {"type":"key","key":"Home","repeat":2}])),
                "Home repeat",
            ),
            (
                steps(json!([text, {"type":"key","key":"Ctrl+U","repeat":2}])),
                "Ctrl+U repeat",
            ),
            (
                steps(json!([text, {"type":"key","key":"Left","repeat":33}])),
                "repeat 33",
            ),
            (
                steps(json!([text, {"type":"text","text":"a\rb"}])),
                "CR in text",
            ),
            (
                steps(json!([text, {"type":"text","text":""}])),
                "empty text",
            ),
            (steps(json!([text, "Left"])), "scalar step"),
            (
                steps(json!([text, {"type":"key","key":"Left","extra":1}])),
                "extra field",
            ),
            (
                json!({"type":"sequence","steps":[text, text],"repeat":2}),
                "repeat on sequence",
            ),
            (json!({"type":"sequence"}), "missing steps"),
            (
                steps(
                    json!([{"type":"text","text":"x".repeat(9000)},{"type":"text","text":"y".repeat(9000)}]),
                ),
                "size",
            ),
        ] {
            assert!(encode(&invalid, &surface).is_err(), "{why}: {invalid}");
        }
        let full = steps(
            json!([{"type":"text","text":"x".repeat(8192)},{"type":"text","text":"y".repeat(8192)}]),
        );
        assert_eq!(encode(&full, &surface).unwrap().bytes().len(), 16384);
    }

    /// #1677 `replace`: the shape check runs where every action's shape is
    /// checked (before any claim) and yields the arguments, not bytes; the
    /// plan comes from the live frame later.
    #[test]
    fn replace_validates_its_shape_and_carries_no_bytes() {
        let surface = surface();
        assert_eq!(
            encode(&json!({"type":"replace","from":"11","to":"19"}), &surface).unwrap(),
            Encoded::Replace {
                from: "11".into(),
                to: "19".into()
            }
        );
        assert_eq!(
            encode(&json!({"type":"replace","from":"松果","to":""}), &surface).unwrap(),
            Encoded::Replace {
                from: "松果".into(),
                to: String::new()
            },
            "an empty to deletes"
        );
        let long = "y".repeat(200);
        assert!(encode(&json!({"type":"replace","from":long,"to":"x"}), &surface).is_ok());
        for (invalid, why) in [
            (json!({"type":"replace","from":"","to":"x"}), "empty from"),
            (
                json!({"type":"replace","from":"y".repeat(201),"to":"x"}),
                "201-byte from",
            ),
            (
                json!({"type":"replace","from":"a\rb","to":"x"}),
                "CR in from",
            ),
            (json!({"type":"replace","from":"a","to":"x\n"}), "LF in to"),
            (json!({"type":"replace","from":"a","to":"\t"}), "tab in to"),
            (json!({"type":"replace","from":"a"}), "missing to"),
            (json!({"type":"replace","to":"a"}), "missing from"),
            (json!({"type":"replace","from":1,"to":"a"}), "numeric from"),
            (json!({"type":"replace","from":"a","to":null}), "null to"),
            (
                json!({"type":"replace","from":"a","to":"b","repeat":2}),
                "extra field",
            ),
            (
                json!({"type":"replace","from":"a","to":"x".repeat(16385)}),
                "to past the action bound",
            ),
            (
                json!({"type":"sequence","steps":[{"type":"replace","from":"a","to":"b"},{"type":"text","text":"x"}]}),
                "replace inside a sequence",
            ),
        ] {
            assert!(encode(&invalid, &surface).is_err(), "{why}: {invalid}");
        }
        assert_eq!(
            sequence_steps(&json!({"type":"replace","from":"a","to":"b"})),
            None
        );
    }

    /// #1666 r1: the below-cursor tolerance admits draft edits only
    /// (#1677: `replace` included).
    #[test]
    fn edits_the_draft_admits_text_sequence_and_editing_keys_only() {
        for action in [
            json!({"type":"text","text":"abc"}),
            json!({"type":"sequence","steps":[{"type":"text","text":"a"},{"type":"key","key":"Left"}]}),
            json!({"type":"replace","from":"11","to":"19"}),
            json!({"type":"key","key":"Backspace"}),
            json!({"type":"key","key":"Ctrl+U","repeat":1}),
        ] {
            assert!(edits_the_draft(&action), "{action}");
        }
        for key in SEQUENCE_KEYS {
            assert!(edits_the_draft(&json!({"type":"key","key":key})), "{key}");
        }
        for action in [
            json!({"type":"submit","text":"abc"}),
            json!({"type":"click","column":0,"row":0}),
            json!({"type":"key","key":"Enter"}),
            json!({"type":"key","key":"Tab"}),
            json!({"type":"key","key":"Escape"}),
            json!({"type":"key","key":"Ctrl+C"}),
            json!({"type":"key","key":"Ctrl+J"}),
            json!({"type":"key","key":"PageUp"}),
            json!({"type":"key"}),
            json!({"type":"unknown"}),
            json!("text"),
        ] {
            assert!(!edits_the_draft(&action), "{action}");
        }
        assert!(BELOW_CURSOR_EDITS_ONLY.contains("text, sequence, replace and editing keys"));
    }
}
