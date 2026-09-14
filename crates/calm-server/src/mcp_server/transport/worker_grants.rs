//! Frozen per-task plugin grants, intersected with the live platform admission.
use super::*;
use crate::track_report::dispatch::PluginToolAdmission;

/// None means a legacy Worker; an isolated task with no grants is Some(empty).
async fn isolated_grants(
    ctx: &Arc<AppContext>,
    identity: &ToolCallIdentity,
) -> Result<Option<Vec<String>>, RpcError> {
    if identity.role != CardRole::Worker {
        return Ok(None);
    }
    let track_id = identity
        .track_id
        .as_deref()
        .ok_or_else(|| RpcError::method_not_found("worker tool grants"))?;
    crate::isolated_codex::lookup::delegated_plugin_tools(
        ctx.repo.as_ref(),
        &identity.card_id,
        &identity.session_id,
        track_id,
    )
    .await
    .map_err(|e| RpcError::internal(format!("isolated tool grant binding: {e}")))
}

fn native_tool(name: &str) -> bool {
    crate::dedicated_codex::MCP_TOOL_ALLOWLIST.contains(&name)
}

/// Exact ordinary tools in the current Track. Never infer grants from annotations.
async fn eligible_plugin_tools(
    ctx: &Arc<AppContext>,
    track_id: Option<&str>,
) -> Result<BTreeSet<String>, RpcError> {
    let Some(host) = ctx.plugin_host.get().cloned() else {
        return Ok(BTreeSet::new());
    };
    let running = host.running_plugin_ids().await;
    let scope = plugin_scope_for_track(ctx, track_id).await;
    eligible_plugin_tools_from(host.registry(), &running, &scope)
}

fn eligible_plugin_tools_from(
    registry: &crate::plugin_host::PluginRegistry,
    running: &BTreeSet<String>,
    scope: &TrackPluginScope,
) -> Result<BTreeSet<String>, RpcError> {
    let mut names = BTreeSet::new();
    for descriptor in plugin_tool_descriptors_from(registry.list(), running, scope) {
        if let Some((_, _, None)) = plugin_tool_route(registry, &descriptor.name, running)? {
            names.insert(descriptor.name);
        }
    }
    Ok(names)
}

/// The spelling Codex shows the model: every char outside `[A-Za-z0-9_]`
/// becomes `_` (codex-mcp `sanitize_responses_api_tool_name`). The model
/// only ever sees this form, so `plugin_tools` may arrive spelled this way.
fn codex_sanitized(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// #1668 — resolve the Planner's requested `plugin_tools` to registry names
/// before the dispatch transaction, and precompute why each unresolvable or
/// ineligible name is refused. Returns the names to freeze (sorted, deduped)
/// and the admission snapshot the report writer checks against.
///
/// Rules per requested name:
/// * an exact registry name (installed, any state) stays as written;
/// * otherwise it is matched by Codex-sanitized equality against the
///   delegable set — one hit resolves, several are ambiguous (`-32602`);
/// * with no delegable hit, one sanitized hit among installed-but-ineligible
///   tools resolves too, so the refusal names the real tool and its reason
///   and a replay after revocation still finds its receipt;
/// * anything else stays as written and is refused as unknown.
pub(crate) async fn resolve_dispatch_plugin_tools(
    ctx: &Arc<AppContext>,
    track_id: Option<&str>,
    requested: &[String],
) -> Result<(Vec<String>, PluginToolAdmission), RpcError> {
    let Some(host) = ctx.plugin_host.get().cloned() else {
        let denied = requested
            .iter()
            .map(|name| (name.clone(), format!("{name} (no plugin host)")))
            .collect();
        return Ok((
            requested.to_vec(),
            PluginToolAdmission {
                eligible: BTreeSet::new(),
                denied,
            },
        ));
    };
    let running = host.running_plugin_ids().await;
    let scope = plugin_scope_for_track(ctx, track_id).await;
    resolve_dispatch_plugin_tools_from(host.registry(), &running, &scope, requested)
}

fn resolve_dispatch_plugin_tools_from(
    registry: &crate::plugin_host::PluginRegistry,
    running: &BTreeSet<String>,
    scope: &TrackPluginScope,
    requested: &[String],
) -> Result<(Vec<String>, PluginToolAdmission), RpcError> {
    let eligible = eligible_plugin_tools_from(registry, running, scope)?;
    // Every minted name the registry knows, whatever its running state,
    // scope or kind: the universe the refusal reasons are computed over.
    let installed: BTreeSet<String> = registry.list().into_iter().map(|m| m.id).collect();
    let minted: BTreeSet<String> =
        plugin_tool_descriptors_from(registry.list(), &installed, &TrackPluginScope::All)
            .into_iter()
            .map(|d| d.name)
            .collect();
    let reason = |name: &str| -> Result<String, RpcError> {
        let Some((plugin_id, tool, kind)) = plugin_tool_route(registry, name, &installed)? else {
            return Ok("not an installed plugin tool".into());
        };
        Ok(
            match plugin_tool_entry(registry, running, &plugin_id, &tool) {
                ToolEntry::Found(_) if !scope.allows(&plugin_id) => "out of track scope".into(),
                ToolEntry::Found(_) if kind.is_some() => "execution-backed".into(),
                ToolEntry::Found(_) => "not delegable".into(),
                miss => miss
                    .miss_reason(&plugin_id, &tool)
                    .unwrap_or_else(|| "not delegable".into()),
            },
        )
    };
    let sanitized_hits = |pool: &BTreeSet<String>, key: &str| -> Vec<String> {
        pool.iter()
            .filter(|candidate| codex_sanitized(candidate) == key)
            .cloned()
            .collect()
    };
    let mut resolved = Vec::with_capacity(requested.len());
    let mut denied = std::collections::BTreeMap::new();
    for name in requested {
        if minted.contains(name) {
            if !eligible.contains(name) {
                denied.insert(name.clone(), format!("{name} ({})", reason(name)?));
            }
            resolved.push(name.clone());
            continue;
        }
        let key = codex_sanitized(name);
        let hits = sanitized_hits(&eligible, &key);
        match hits.as_slice() {
            [real] => resolved.push(real.clone()),
            [] => {
                let installed_hits = sanitized_hits(&minted, &key);
                if let [real] = installed_hits.as_slice() {
                    denied.insert(
                        real.clone(),
                        format!("{name} (resolves to {real}: {})", reason(real)?),
                    );
                    resolved.push(real.clone());
                } else {
                    let detail = if installed_hits.is_empty() {
                        "not an installed plugin tool".to_string()
                    } else {
                        let mut parts = Vec::with_capacity(installed_hits.len());
                        for real in &installed_hits {
                            parts.push(format!("{real}: {}", reason(real)?));
                        }
                        format!("matches {}", parts.join("; "))
                    };
                    denied.insert(name.clone(), format!("{name} ({detail})"));
                    resolved.push(name.clone());
                }
            }
            _ => {
                return Err(RpcError::invalid_params(format!(
                    "plugin_tools entry `{name}` is ambiguous; use one exact registry name: {}",
                    hits.join(", ")
                )));
            }
        }
    }
    resolved.sort();
    resolved.dedup();
    Ok((resolved, PluginToolAdmission { eligible, denied }))
}

pub(super) async fn filter(
    ctx: &Arc<AppContext>,
    identity: &ToolCallIdentity,
    descriptors: &mut Vec<ToolDescriptor>,
) -> Result<(), RpcError> {
    if let Some(grants) = isolated_grants(ctx, identity).await? {
        let eligible = eligible_plugin_tools(ctx, identity.track_id.as_deref()).await?;
        descriptors.retain(|d| {
            native_tool(&d.name) || (grants.contains(&d.name) && eligible.contains(&d.name))
        });
    }
    Ok(())
}

pub(super) async fn require(
    ctx: &Arc<AppContext>,
    identity: &ToolCallIdentity,
    name: &str,
) -> Result<(), RpcError> {
    if let Some(grants) = isolated_grants(ctx, identity).await? {
        if native_tool(name) {
            return Ok(());
        }
        let eligible = eligible_plugin_tools(ctx, identity.track_id.as_deref()).await?;
        if !grants.iter().any(|g| g == name) || !eligible.contains(name) {
            return Err(RpcError::method_not_found(&format!("tools/call: {name}")));
        }
    }
    Ok(())
}

/// #1668 — the pure resolver against a registry in the fixture shape of
/// `tests/cases/mcp_plugin_tools.rs`: `dev.echo_do.thing` and
/// `dev_echo.do.thing` collide once Codex sanitizes them, and the trusted
/// plugin carries `-`/`.` in its id plus an execution-backed tool.
#[cfg(test)]
mod dispatch_resolution_tests {
    use super::*;
    use crate::plugin_host::{Manifest, PluginRegistry};

    const DOTTED: &str = "plugin.dev.echo_do.thing";
    const COLLIDING: &str = "plugin.dev_echo.do.thing";
    const TRUSTED_ID: &str = "dev.neige.git-forge";
    const TRUSTED: &str = "plugin.dev.neige.git-forge_wf.tool";
    const FORGE_ACTION: &str = "plugin.dev.neige.git-forge_execute";

    fn manifest(id: &str, tools: Value) -> Manifest {
        Manifest::parse(
            &json!({
                "manifest_version": 1, "id": id, "version": "0.1.0", "min_kernel_version": "0.0.1",
                "display_name": id, "entrypoint": {"command": "bin/stub"},
                "exposes_tools": tools, "permissions": {}
            })
            .to_string(),
        )
        .expect("fixture manifest parses")
    }

    fn registry() -> PluginRegistry {
        PluginRegistry::from_manifests([
            (manifest("dev.echo", json!([{"name": "do.thing"}])), None),
            (manifest("dev", json!([{"name": "echo.do.thing"}])), None),
            (
                manifest(
                    TRUSTED_ID,
                    json!([{"name": "wf.tool"}, {"name": "execute", "kind": "forge-action"}]),
                ),
                None,
            ),
        ])
    }

    fn running(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    fn resolve(
        running_ids: &[&str],
        scope: TrackPluginScope,
        requested: &[&str],
    ) -> Result<(Vec<String>, PluginToolAdmission), RpcError> {
        let requested: Vec<String> = requested.iter().map(|s| s.to_string()).collect();
        resolve_dispatch_plugin_tools_from(&registry(), &running(running_ids), &scope, &requested)
    }

    const ALL: &[&str] = &["dev.echo", "dev", TRUSTED_ID];

    #[test]
    fn codex_sanitized_matches_the_responses_api_alphabet() {
        assert_eq!(
            codex_sanitized(TRUSTED),
            "plugin_dev_neige_git_forge_wf_tool"
        );
        assert_eq!(codex_sanitized("a_b9Z"), "a_b9Z");
        assert_eq!(codex_sanitized("é-x"), "__x");
    }

    #[test]
    fn sanitized_spellings_resolve_to_the_unique_eligible_registry_name() {
        let (resolved, admission) = resolve(
            ALL,
            TrackPluginScope::All,
            &[
                "plugin.dev_neige_git_forge_wf_tool",
                "plugin_dev_neige_git_forge_wf_tool",
                DOTTED,
            ],
        )
        .unwrap();
        assert_eq!(resolved, vec![DOTTED.to_string(), TRUSTED.to_string()]);
        assert!(admission.denied.is_empty(), "{admission:?}");
        assert!(admission.refusal(&resolved).is_none());
    }

    #[test]
    fn exact_registry_name_is_never_substituted_by_a_sanitized_neighbour() {
        // `dev` is stopped; its tool's sanitized form equals the running
        // `dev.echo` tool's. Written exactly, it must stay itself and be refused.
        let (resolved, admission) = resolve(
            &["dev.echo", TRUSTED_ID],
            TrackPluginScope::All,
            &[COLLIDING],
        )
        .unwrap();
        assert_eq!(resolved, vec![COLLIDING.to_string()]);
        let refusal = admission.refusal(&resolved).expect("refused");
        assert!(
            refusal.contains(&format!("{COLLIDING} (plugin dev is not running)")),
            "{refusal}"
        );
        assert!(!refusal.contains(DOTTED), "{refusal}");
    }

    #[test]
    fn ambiguous_sanitized_spelling_is_invalid_params_listing_candidates() {
        for spelling in ["plugin.dev_echo_do_thing", "plugin_dev_echo_do_thing"] {
            let error = resolve(ALL, TrackPluginScope::All, &[spelling]).unwrap_err();
            assert_eq!(error.code, RpcError::INVALID_PARAMS, "{error}");
            assert!(error.message.contains(spelling), "{error}");
            assert!(error.message.contains(DOTTED), "{error}");
            assert!(error.message.contains(COLLIDING), "{error}");
        }
        // Once only one of the pair is delegable the spelling is unique again.
        let (resolved, admission) = resolve(
            &["dev", TRUSTED_ID],
            TrackPluginScope::All,
            &["plugin.dev_echo_do_thing"],
        )
        .unwrap();
        assert_eq!(resolved, vec![COLLIDING.to_string()]);
        assert!(admission.refusal(&resolved).is_none());
    }

    #[test]
    fn sanitized_spelling_of_a_stopped_tool_resolves_and_is_refused_by_name() {
        let (resolved, admission) = resolve(
            &["dev.echo", "dev"],
            TrackPluginScope::All,
            &["plugin.dev_neige_git_forge_wf_tool"],
        )
        .unwrap();
        assert_eq!(resolved, vec![TRUSTED.to_string()]);
        let refusal = admission.refusal(&resolved).expect("refused");
        assert_eq!(
            refusal,
            format!(
                "plugin_tools not delegable: plugin.dev_neige_git_forge_wf_tool \
                 (resolves to {TRUSTED}: plugin {TRUSTED_ID} is not running)"
            )
        );
    }

    #[test]
    fn refusal_names_every_ineligible_tool_with_its_reason() {
        let (resolved, admission) = resolve(
            ALL,
            TrackPluginScope::Only("dev".into()),
            &[
                FORGE_ACTION,
                DOTTED,
                COLLIDING,
                "plugin.nope_x",
                "calm_report_read",
                "plugin.dev_echo_do_thing",
            ],
        )
        .unwrap();
        // Only `dev` is in scope, so the collision resolves to its tool.
        assert!(resolved.contains(&COLLIDING.to_string()), "{resolved:?}");
        let refusal = admission.refusal(&resolved).expect("refused");
        assert!(
            refusal.starts_with("plugin_tools not delegable: "),
            "{refusal}"
        );
        for expected in [
            &format!("{FORGE_ACTION} (out of track scope)"),
            &format!("{DOTTED} (out of track scope)"),
            "plugin.nope_x (not an installed plugin tool)",
            "calm_report_read (not an installed plugin tool)",
        ] {
            assert!(
                refusal.contains(expected),
                "missing `{expected}` in {refusal}"
            );
        }
        assert!(!refusal.contains(COLLIDING), "{refusal}");
        // Execution-backed is reported when scope is not the blocker.
        let (resolved, admission) = resolve(ALL, TrackPluginScope::All, &[FORGE_ACTION]).unwrap();
        let refusal = admission.refusal(&resolved).expect("refused");
        assert_eq!(
            refusal,
            format!("plugin_tools not delegable: {FORGE_ACTION} (execution-backed)")
        );
        // Both stopped: the collision cannot resolve, and the refusal says why for each.
        let (resolved, admission) = resolve(
            &[TRUSTED_ID],
            TrackPluginScope::All,
            &["plugin.dev_echo_do_thing"],
        )
        .unwrap();
        assert_eq!(resolved, vec!["plugin.dev_echo_do_thing".to_string()]);
        let refusal = admission.refusal(&resolved).expect("refused");
        assert!(
            refusal.contains(&format!(
                "plugin.dev_echo_do_thing (matches {DOTTED}: plugin dev.echo is not running; {COLLIDING}: plugin dev is not running)"
            )),
            "{refusal}"
        );
    }
}
