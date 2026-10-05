//! #2003 §3.2: the callable names the deployed `codex-cli 0.159.2` actually minted for the served
//! names below (captured from its Responses request), fed to `neige_source_capture`. Served names
//! are in `[A-Za-z0-9_]` (#2087 B5), so Codex only cuts and hashes a name over its byte cap or two
//! equal names; the captures were made under the `-` spelling, which sanitizes to the same name.

#![cfg(unix)]

use serde_json::json;

use crate::mcp_source_capture::{assert_invalid_params, capture, ok_result, record};
use crate::mcp_track_report::boot;

/// An installed external id of the legacy shape: it mints `plugin_dev_neige_git_forge_<tool>`.
const FORGE: &str = "dev-neige-git-forge";

/// Codex cut these to 104 bytes and appended `_` + 12 hex: one per raw tool `x` × 91, 92, 93,
/// 101, 102, 113 and 173 (raw names of 118 to 200 bytes).
const TRUNCATED_CALLABLES: [&str; 7] = [
    "plugin_dev_neige_git_forge_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx_013580773a32",
    "plugin_dev_neige_git_forge_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx_2cea2406a776",
    "plugin_dev_neige_git_forge_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx_3c3ef146471c",
    "plugin_dev_neige_git_forge_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx_ade08737ca31",
    "plugin_dev_neige_git_forge_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx_b1bd161b778c",
    "plugin_dev_neige_git_forge_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx_bc3150ddd212",
    "plugin_dev_neige_git_forge_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx_fff2e31e6169",
];
const HASHED_RAW_TOOL_LENGTHS: [usize; 7] = [91, 92, 93, 101, 102, 113, 173];

/// `dev.x-y` and `dev.x.y` both mint `plugin_dev_x_y_t`; the host refuses to run both, and Codex
/// hash-suffixed both callables when it was served the two raw spellings.
const COLLIDING_CALLABLES: [&str; 2] = [
    "plugin_dev_x_y_t_7a37e287c14a",
    "plugin_dev_x_y_t_ec721b457ce9",
];

/// The longest raw tool Codex left unhashed: 117 bytes, its callable is the sanitized raw name.
const UNHASHED_TOOL_LENGTH: usize = 90;

/// A hashed callable resolves to no tool and is refused as unknown, listing the minted names that
/// do have a record; it never routes to a recorded tool. Unhashed callables still resolve.
#[tokio::test]
async fn hashed_codex_callables_fail_explicitly() {
    let boot = boot().await;
    let mut raw_names = Vec::new();
    for len in HASHED_RAW_TOOL_LENGTHS
        .iter()
        .chain(std::iter::once(&UNHASHED_TOOL_LENGTH))
    {
        let tool = "x".repeat(*len);
        record(
            &boot,
            FORGE,
            &tool,
            &json!({}),
            &ok_result(&[&format!("body-{len}")]),
        );
        raw_names.push(format!("plugin_dev_neige_git_forge_{tool}"));
    }
    for plugin in ["dev.x-y", "dev.x.y"] {
        record(&boot, plugin, "t", &json!({}), &ok_result(&[plugin]));
        raw_names.push("plugin_dev_x_y_t".to_string());
    }

    for bare in TRUNCATED_CALLABLES.iter().chain(COLLIDING_CALLABLES.iter()) {
        assert!(
            bare.len() <= 117,
            "{bare} is a captured callable under `mcp__neige`"
        );
        for probe in [bare.to_string(), format!("mcp__neige__{bare}")] {
            let err = capture(
                &boot,
                json!({ "call": { "tool": probe }, "provenance": "summary", "title": "x" }),
            )
            .await
            .expect_err("a hashed callable must not resolve");
            assert_invalid_params(&err, "unknown tool name");
            for raw in &raw_names {
                assert!(
                    err.message.contains(raw.as_str()),
                    "{raw} not listed: {err}"
                );
            }
        }
    }

    let unhashed = format!(
        "plugin_dev_neige_git_forge_{}",
        "x".repeat(UNHASHED_TOOL_LENGTH)
    );
    assert_eq!(unhashed.len(), 117);
    for probe in [unhashed.clone(), format!("mcp__neige__{unhashed}")] {
        let receipt = capture(
            &boot,
            json!({ "call": { "tool": probe }, "provenance": "summary", "title": "x" }),
        )
        .await
        .expect("an unhashed callable resolves");
        assert_eq!(receipt["matched_call"]["tool"], unhashed);
    }
}
