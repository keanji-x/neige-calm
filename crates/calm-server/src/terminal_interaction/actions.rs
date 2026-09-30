//! Input action validation and encoding against the live input surface. Every action is one
//! ordered write request: one barrier, one acknowledgement, one receipt.
use anyhow::{Result, ensure};
use calm_terminal_view::{InputSurface, key_bytes};
use serde_json::Value;

/// Bound on the bytes one action may write (text and sequences alike).
pub const ACTION_BYTES_MAX: usize = 16384;
/// A sequence carries 2..=8 steps.
pub const SEQUENCE_STEPS_MIN: usize = 2;
pub const SEQUENCE_STEPS_MAX: usize = 8;
/// Keys a sequence step may send: cursor movement and draft editing only, so a sequence can
/// never carry a CR or an LF.
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

/// What one action writes: its bytes, or a `submit` (text plus one CR, handed to the PTY as two
/// physical writes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Encoded {
    Bytes(Vec<u8>),
    Submit(Vec<u8>),
}
impl Encoded {
    /// The bytes of an action that carries them (tests).
    #[cfg(test)]
    fn bytes(self) -> Vec<u8> {
        match self {
            Self::Bytes(bytes) | Self::Submit(bytes) => bytes,
        }
    }
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
/// One step of a sequence: a `text` or a `key` action with the sequence key vocabulary.
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
pub fn encode(action: &Value, surface: &InputSurface) -> Result<Encoded> {
    let object = action
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("terminal action must be an object"))?;
    let bytes = match action["type"].as_str() {
        Some("text") => Ok(printable_text(action, object, "text")?.as_bytes().to_vec()),
        Some("submit") => {
            // The writer hands the CR to the PTY as a second write after the text. Explicit opt-in;
            // `text` alone never submits and `submit` never repeats.
            let mut bytes = printable_text(action, object, "submit")?
                .as_bytes()
                .to_vec();
            bytes.push(b'\r');
            return Ok(Encoded::Submit(bytes));
        }
        Some("key") => encode_key(action, object, surface, None),
        Some("sequence") => {
            // The step encodings concatenated in one ordered write request; no CR and no LF possible
            // (the key vocabulary has neither).
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
        for retired in [
            json!({"type":"replace","from":"a","to":"b"}),
            json!({"type":"click","column":0,"row":0}),
        ] {
            assert!(encode(&retired, &surface).is_err(), "{retired}");
        }
    }

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
}
