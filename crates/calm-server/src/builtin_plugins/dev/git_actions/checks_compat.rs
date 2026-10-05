//! Byte-frozen pre-2129 descriptor, used for identity only, never execution.
use super::*;
use crate::mcp_server::transport::{PluginForgePayload, semantic_payload_hash};

// Preserve these bytes: they were included in the released semantic hash.
const PRE_2129_FOLD: &str = "{conclusion: ([(.statusCheckRollup // [])[] | if .__typename == \"CheckRun\" then (if .status != \"COMPLETED\" then \"pending\" elif .conclusion == \"SUCCESS\" or .conclusion == \"NEUTRAL\" or .conclusion == \"SKIPPED\" then \"success\" else \"failure\" end) elif .state == \"SUCCESS\" then \"success\" elif .state == \"PENDING\" or .state == \"EXPECTED\" then \"pending\" else \"failure\" end] | if any(. == \"failure\") then \"failure\" elif any(. == \"pending\") then \"pending\" elif length == 0 then \"no_checks\" else \"success\" end), mergeable: (.mergeable | ascii_downcase), head_sha: .headRefOid}";

pub(super) fn predecessor_hash(payload: &Value, repo: &str, pr: u64) -> Result<String, String> {
    let mut legacy: PluginForgePayload = serde_json::from_value(payload.clone())
        .map_err(|e| format!("decode checks identity: {e}"))?;
    let fields = &mut legacy
        .event_spec
        .as_mut()
        .expect("checks event extractor")
        .fields;
    fields.remove("snapshot");
    fields.remove("failed_checks");
    legacy
        .probe
        .as_mut()
        .expect("checks probe")
        .output_probe_argv = Some(vec![
        "gh".into(),
        "pr".into(),
        "view".into(),
        pr.to_string(),
        "--repo".into(),
        repo.into(),
        "--json".into(),
        "headRefOid,mergeable,statusCheckRollup".into(),
        "--jq".into(),
        PRE_2129_FOLD.into(),
    ]);
    semantic_payload_hash(&legacy).map_err(|e| format!("hash checks predecessor: {e:?}"))
}
