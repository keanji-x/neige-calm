//! Trusted caller identity delivered to local forge-action lowerers.
use serde::{Deserialize, Serialize};

pub const FORGE_CALLER_META_KEY: &str = "dev.neige/forge-caller";

/// Matches the kernel's idempotency namespace. Every field comes from dispatch,
/// never from tool arguments; lowerers can bind remote recovery to this caller.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ForgeCallerScope {
    pub plugin_id: String,
    pub track_id: String,
    pub card_id: String,
}

impl ForgeCallerScope {
    pub fn validate(&self) -> Result<(), String> {
        for (name, value) in [
            ("plugin_id", &self.plugin_id),
            ("track_id", &self.track_id),
            ("card_id", &self.card_id),
        ] {
            if value.trim().is_empty() {
                return Err(format!("forge caller `{name}` must not be blank"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_host::mcp::{
        InitializeMeta, KERNEL_PROTOCOL_VERSION, McpClient, TRACK_META_KEY, TrackMeta,
    };
    use serde_json::{Value, json};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    #[tokio::test]
    async fn forge_call_metadata_is_separate_from_user_arguments() {
        let (kernel, plugin) = tokio::io::duplex(8192);
        let (kernel_read, kernel_write) = tokio::io::split(kernel);
        let (plugin_read, mut plugin_write) = tokio::io::split(plugin);
        let task = tokio::spawn(async move {
            let mut lines = BufReader::new(plugin_read).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let frame: Value = serde_json::from_str(&line).unwrap();
                let Some(id) = frame.get("id") else {
                    continue;
                };
                let result = if frame["method"] == "initialize" {
                    json!({"protocolVersion":KERNEL_PROTOCOL_VERSION,"serverInfo":{"name":"stub","version":"0"},"capabilities":{}})
                } else {
                    assert_eq!(frame["method"], "tools/call");
                    let params = &frame["params"];
                    assert_eq!(
                        params["_meta"][FORGE_CALLER_META_KEY],
                        json!({"plugin_id":"plugin-a","track_id":"track-a","card_id":"card-a"})
                    );
                    assert_eq!(params["_meta"][TRACK_META_KEY]["id"], "track-a");
                    assert_eq!(
                        params["arguments"][FORGE_CALLER_META_KEY]["card_id"],
                        "spoofed-card"
                    );
                    json!({"content":[],"isError":false})
                };
                let reply = json!({"jsonrpc":"2.0","id":id,"result":result});
                plugin_write
                    .write_all(format!("{reply}\n").as_bytes())
                    .await
                    .unwrap();
                plugin_write.flush().await.unwrap();
                if frame["method"] == "tools/call" {
                    break;
                }
            }
        });
        let client = McpClient::connect_with_auth(
            kernel_read,
            kernel_write,
            InitializeMeta {
                expected_echo: None,
                config: None,
            },
        )
        .await
        .unwrap();
        let caller = ForgeCallerScope {
            plugin_id: "plugin-a".into(),
            track_id: "track-a".into(),
            card_id: "card-a".into(),
        };
        let track = TrackMeta {
            id: "track-a".into(),
            creator_track_id: None,
            creator_key: None,
        };
        let result = client
            .forge_tools_call(
                "tool-a",
                json!({FORGE_CALLER_META_KEY:{"card_id":"spoofed-card"}}),
                &track,
                &caller,
            )
            .await
            .unwrap();
        assert_eq!(result.is_error, Some(false));
        task.await.unwrap();
    }
}
