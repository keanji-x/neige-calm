use super::*;
use calm_types::forge_action::SUPPORTED_FORGE_EVENT_KINDS;

pub(super) fn assert_no_reserved_context(payload: &Value, reserved: &[&str]) {
    let context = payload["context"].as_object().expect("context object");
    for key in reserved {
        assert!(
            !context.contains_key(*key),
            "context must not contain reserved key `{key}`"
        );
    }
    if let Some(fields) = payload
        .pointer("/event_spec/fields")
        .and_then(Value::as_object)
    {
        for key in reserved {
            assert!(
                !fields.contains_key(*key),
                "event fields must not contain reserved key `{key}`"
            );
        }
    }
}

pub(super) fn run_git<const N: usize>(cwd: &std::path::Path, args: [&str; N]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git failed: status={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

pub(super) fn is_hex_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(super) fn assert_supported_event_kind(payload: &Value) {
    let event_kind = payload
        .pointer("/event_spec/event_kind")
        .and_then(Value::as_str)
        .expect("payload carries event kind");
    assert!(
        SUPPORTED_FORGE_EVENT_KINDS.contains(&event_kind),
        "unsupported event kind `{event_kind}`"
    );
}
