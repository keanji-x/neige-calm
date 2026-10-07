//! Admission conflict checks, callback pumps and supervisor accounting.
use super::*;

/// The crash-window counters a new live entry starts from: `inherit` wins, else the entry being replaced (an explicit spawn after a crash must not zero them).
pub(super) fn inherited_window(
    table: &ProcessTable,
    id: &str,
    inherit: Option<CrashWindow>,
) -> (u32, Instant) {
    match inherit {
        Some(w) => (w.crashes, w.started),
        None => match table.live.get(id) {
            Some(prev) => (prev.crashes_in_window, prev.window_started),
            None => (0, Instant::now()),
        },
    }
}

/// The first running or admitted plugin that already mints a name `id` would serve: the same
/// `plugin_<id>_` prefix, or one of `tools`' `plugin_<id>_<tool>` names.
/// Skips compiled natives; safe as built-in ids are reserved (registry.rs:173, lifecycle.rs:128).
pub(super) fn find_minted_name_conflict(
    id: &str,
    tools: &[manifest::ExposedTool],
    holders: &BTreeSet<String>,
    registry: Vec<Manifest>,
) -> Option<HostError> {
    use crate::results::{PLUGIN_TOOL_PREFIX, minted_segment, registry_name};
    let prefix = minted_segment(id);
    let own: BTreeSet<String> = tools.iter().map(|t| registry_name(id, &t.name)).collect();
    let conflict = |held_by: &str, minted: String| HostError::MintedNameConflict {
        plugin_id: id.to_string(),
        held_by: held_by.to_string(),
        minted,
    };
    let mut others: Vec<Manifest> = registry
        .into_iter()
        .filter(|other| other.id != id && holders.contains(&other.id))
        .collect();
    others.sort_by(|a, b| a.id.cmp(&b.id));
    for other in others {
        if minted_segment(&other.id) == prefix {
            return Some(conflict(
                &other.id,
                format!("{PLUGIN_TOOL_PREFIX}{prefix}_"),
            ));
        }
        if let Some(minted) = other
            .exposes_tools
            .iter()
            .map(|t| registry_name(&other.id, &t.name))
            .find(|minted| own.contains(minted))
        {
            return Some(conflict(&other.id, minted));
        }
    }
    None
}

/// Pure core of the template-id uniqueness check: only trusted plugins participate, only holders (running + admission-reserved) count, and the plugin's own registry entry is skipped.
/// The trust predicate is injected to keep this testable without process env.
pub(super) fn find_template_conflict(
    manifest: &Manifest,
    candidates: impl IntoIterator<Item = Manifest>,
    holder_ids: &BTreeSet<String>,
    is_trusted: &dyn Fn(&str) -> bool,
) -> Option<HostError> {
    if !is_trusted(&manifest.id) {
        return None;
    }
    for template in &manifest.templates {
        if let Some(owner) = crate::builtin::required_owner(&template.id)
            && owner != manifest.id
        {
            return Some(HostError::TemplateConflict {
                plugin_id: manifest.id.clone(),
                template_id: template.id.clone(),
                held_by: owner.into(),
            });
        }
    }
    for other in candidates {
        if other.id == manifest.id || !holder_ids.contains(&other.id) || !is_trusted(&other.id) {
            continue;
        }
        for template in &manifest.templates {
            if other.templates.iter().any(|held| held.id == template.id) {
                return Some(HostError::TemplateConflict {
                    plugin_id: manifest.id.clone(),
                    template_id: template.id.clone(),
                    held_by: other.id.clone(),
                });
            }
        }
    }
    None
}

/// Router task: drains inbound MCP requests into `callbacks::dispatch`; notifications are logged and dropped. Ends when both channels close.
#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_neige_router(
    plugin_id: String,
    callbacks: Arc<dyn Callbacks>,
    registry: Arc<PluginRegistry>,
    mcp: Arc<McpClient>,
    subscriptions: Arc<Mutex<Vec<SubscriptionRecord>>>,
    mut inbound: mpsc::Receiver<InboundRequest>,
    inbound_notifs: Option<mpsc::Receiver<InboundNotification>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // Notifications are lossy by specification; logged for debugging only.
        if let Some(mut notif_rx) = inbound_notifs {
            let plugin_id_n = plugin_id.clone();
            tokio::spawn(async move {
                while let Some(notif) = notif_rx.recv().await {
                    tracing::debug!(
                        plugin_id = %plugin_id_n,
                        method = %notif.method,
                        "inbound plugin notification (currently logged + ignored)"
                    );
                }
            });
        }

        while let Some(req) = inbound.recv().await {
            let outcome = callbacks
                .dispatch(
                    CallbackInvocation {
                        plugin_id: plugin_id.clone(),
                        registry: registry.clone(),
                        mcp: mcp.clone(),
                        subscriptions: subscriptions.clone(),
                        call_id: None,
                    },
                    &req.method,
                    req.params,
                )
                .await;
            // If the responder is gone (plugin disconnected mid-call), drop
            // silently — the mcp reader already cleans up the wire.
            let _ = req.responder.send(outcome);
        }
        tracing::debug!(plugin_id = %plugin_id, "inbound request channel closed");
    })
}

/// Drainer installed when a plugin omits the `experimental.dev.neige/kernel-callbacks` capability: every inbound request gets `MethodNotFound` instead of a hang.
pub(super) fn spawn_methodnotfound_drainer(
    plugin_id: String,
    mut inbound: mpsc::Receiver<InboundRequest>,
    inbound_notifs: Option<mpsc::Receiver<InboundNotification>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Some(mut notif_rx) = inbound_notifs {
            let plugin_id_n = plugin_id.clone();
            tokio::spawn(async move {
                while let Some(notif) = notif_rx.recv().await {
                    tracing::debug!(
                        plugin_id = %plugin_id_n,
                        method = %notif.method,
                        "inbound plugin notification (no-callbacks plugin; logged + ignored)"
                    );
                }
            });
        }
        while let Some(req) = inbound.recv().await {
            let outcome = Err(RpcError::method_not_found(&req.method));
            let _ = req.responder.send(outcome);
        }
        tracing::debug!(plugin_id = %plugin_id, "inbound request channel closed (no-callbacks)");
    })
}

#[cfg(test)]
mod template_conflict_tests {
    use super::*;

    fn manifest_with_template(id: &str, template_id: &str) -> Manifest {
        let json = serde_json::json!({
            "manifest_version": 2,
            "id": id,
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Template Conflict Stub",
            "entrypoint": { "command": "bin/stub" },
            "templates": [
                { "id": template_id }
            ],
            "permissions": {}
        });
        Manifest::parse(&json.to_string()).expect("manifest parses")
    }

    fn running(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    #[test]
    fn duplicate_template_on_running_trusted_plugin_conflicts() {
        let incoming = manifest_with_template("dev.second", "investigation");
        let holder = manifest_with_template("dev.first", "investigation");
        let trusted = |_: &str| true;
        let conflict =
            find_template_conflict(&incoming, [holder], &running(&["dev.first"]), &trusted)
                .expect("duplicate template id must conflict");
        match conflict {
            HostError::TemplateConflict {
                plugin_id,
                template_id,
                held_by,
            } => {
                assert_eq!(plugin_id, "dev.second");
                assert_eq!(template_id, "investigation");
                assert_eq!(held_by, "dev.first");
            }
            other => panic!("expected TemplateConflict, got {other:?}"),
        }
    }

    #[test]
    fn stopped_holder_does_not_squat_on_template_id() {
        let incoming = manifest_with_template("dev.second", "investigation");
        let holder = manifest_with_template("dev.first", "investigation");
        let trusted = |_: &str| true;
        assert!(
            find_template_conflict(&incoming, [holder], &running(&[]), &trusted).is_none(),
            "a stopped plugin must not hold the template id"
        );
    }

    #[test]
    fn untrusted_duplicates_are_tolerated() {
        let incoming = manifest_with_template("dev.second", "investigation");
        let holder = manifest_with_template("dev.first", "investigation");
        let running_ids = running(&["dev.first"]);

        // Untrusted spawner: never enters the resolution set — no conflict.
        let only_first_trusted = |id: &str| id == "dev.first";
        assert!(
            find_template_conflict(
                &incoming,
                [holder.clone()],
                &running_ids,
                &only_first_trusted
            )
            .is_none()
        );

        // Untrusted holder: its templates are unresolvable — no conflict.
        let only_second_trusted = |id: &str| id == "dev.second";
        assert!(
            find_template_conflict(&incoming, [holder], &running_ids, &only_second_trusted)
                .is_none()
        );
    }

    #[test]
    fn respawn_skips_own_registry_entry_and_distinct_ids_pass() {
        let incoming = manifest_with_template("dev.first", "investigation");
        let own_entry = manifest_with_template("dev.first", "investigation");
        let trusted = |_: &str| true;
        assert!(
            find_template_conflict(&incoming, [own_entry], &running(&["dev.first"]), &trusted)
                .is_none(),
            "respawn must not conflict with the plugin's own registry entry"
        );

        let other = manifest_with_template("dev.other", "different-template");
        assert!(
            find_template_conflict(&incoming, [other], &running(&["dev.other"]), &trusted)
                .is_none(),
            "distinct template ids must not conflict"
        );
    }
}

#[cfg(test)]
mod required_template_owner {
    #[test]
    fn required_dev_template_cannot_be_claimed_when_dev_is_disabled() {
        let incoming = super::Manifest::parse(r#"{"manifest_version":2,"id":"other.plugin","version":"0.1.0","min_kernel_version":"0.1.0","display_name":"Other","entrypoint":{"command":"bin/tool"},"templates":[{"id":"dev"}]}"#).unwrap();
        let error = super::find_template_conflict(
            &incoming,
            [],
            &std::collections::BTreeSet::new(),
            &|_| true,
        )
        .unwrap();
        assert!(
            matches!(error, super::HostError::TemplateConflict { ref held_by, .. } if held_by == "gitforge")
        );
    }
}

#[cfg(test)]
mod minted_name_conflict_tests {
    use super::*;

    fn app(id: &str, tools: &[&str]) -> Manifest {
        let tools: Vec<_> = tools
            .iter()
            .map(|name| serde_json::json!({ "name": name }))
            .collect();
        Manifest::parse(
            &serde_json::json!({
                "manifest_version": 1, "id": id, "version": "0.1.0",
                "min_kernel_version": "0.0.1", "display_name": "Stub",
                "entrypoint": { "command": "bin/stub" }, "exposes_tools": tools,
            })
            .to_string(),
        )
        .expect("manifest parses")
    }

    fn holders(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    /// #2087 §6: a second running plugin whose id mints the same prefix (`ab-c`, `ab.c`), or whose
    /// tool mints a running plugin's name (`ab` + `c_d`, `ab-c` + `d`), is refused; a stopped
    /// holder or a distinct name is not.
    #[test]
    fn minted_tool_names_refuse_collisions_across_running_plugins() {
        let held = app("ab-c", &["d"]);
        let same_prefix = app("ab.c", &["z"]);
        let conflict = find_minted_name_conflict(
            "ab.c",
            &same_prefix.exposes_tools,
            &holders(&["ab-c"]),
            vec![held.clone(), same_prefix.clone()],
        )
        .expect("same prefix");
        assert_eq!(
            conflict.to_string(),
            "plugin `ab.c` mints `plugin_ab_c_`, which running plugin `ab-c` already mints"
        );

        let same_name = app("ab", &["c_d", "e"]);
        let conflict = find_minted_name_conflict(
            "ab",
            &same_name.exposes_tools,
            &holders(&["ab-c"]),
            vec![held.clone(), same_name.clone()],
        )
        .expect("same minted tool name");
        assert!(
            matches!(&conflict, HostError::MintedNameConflict { held_by, minted, .. }
                if held_by == "ab-c" && minted == "plugin_ab_c_d"),
            "{conflict:?}"
        );
        assert!(
            find_minted_name_conflict(
                "ab",
                &same_name.exposes_tools,
                &holders(&[]),
                vec![held.clone()]
            )
            .is_none(),
            "a stopped holder does not conflict"
        );
        let distinct = app("ab", &["e"]);
        assert!(
            find_minted_name_conflict(
                "ab",
                &distinct.exposes_tools,
                &holders(&["ab-c"]),
                vec![held]
            )
            .is_none()
        );
    }
}
