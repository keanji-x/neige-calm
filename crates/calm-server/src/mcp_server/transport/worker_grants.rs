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
/// `frozen` is what an earlier dispatch under the same Track-local `name`
/// froze (empty when there is no receipt): a sanitized replay must resolve
/// against that first, or a revoked plugin's still-delegable collider would
/// win and the contract compare would conflict instead of replaying.
///
/// Rules per requested name:
/// * a frozen name or a Track-visible registry name stays as written;
/// * otherwise Codex-sanitized equality, first against `frozen`, then
///   against the delegable set — one hit resolves, several are ambiguous
///   (`-32602`);
/// * with no such hit, one sanitized hit among visible-but-ineligible tools
///   resolves too, so the refusal names the real tool and its reason;
/// * anything else stays as written (a leading `plugin_` rewritten to
///   `plugin.` so validation lets the named refusal through) and is refused
///   as `unknown tool`.
///
/// The universe for sanitized matching and for refusal reasons is the
/// Track-VISIBLE set — every tool of every plugin `scope` allows, whatever
/// its running state or kind — never the whole registry. Out-of-scope and
/// nonexistent names get the byte-identical `unknown tool` wording, the
/// #891 non-disclosure contract `dispatch_plugin_tools_call` keeps for
/// `tools/call`: a bound Track cannot probe other plugins through dispatch.
pub(crate) async fn resolve_dispatch_plugin_tools(
    ctx: &Arc<AppContext>,
    track_id: Option<&str>,
    frozen: &[String],
    requested: &[String],
) -> Result<(Vec<String>, PluginToolAdmission), RpcError> {
    let Some(host) = ctx.plugin_host.get().cloned() else {
        let denied = requested
            .iter()
            .map(|name| (name.clone(), format!("{name} ({UNKNOWN_TOOL})")))
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
    resolve_dispatch_plugin_tools_from(host.registry(), &running, &scope, frozen, requested)
}

/// One wording for every name the Track cannot see: out of scope, not
/// installed, or not a plugin tool at all.
const UNKNOWN_TOOL: &str = "unknown tool";

fn resolve_dispatch_plugin_tools_from(
    registry: &crate::plugin_host::PluginRegistry,
    running: &BTreeSet<String>,
    scope: &TrackPluginScope,
    frozen: &[String],
    requested: &[String],
) -> Result<(Vec<String>, PluginToolAdmission), RpcError> {
    let eligible = eligible_plugin_tools_from(registry, running, scope)?;
    let in_scope: BTreeSet<String> = registry
        .list()
        .into_iter()
        .map(|m| m.id)
        .filter(|id| scope.allows(id))
        .collect();
    let visible: BTreeSet<String> = plugin_tool_descriptors_from(registry.list(), &in_scope, scope)
        .into_iter()
        .map(|d| d.name)
        .collect();
    let reason = |name: &str| -> Result<String, RpcError> {
        if !visible.contains(name) {
            return Ok(UNKNOWN_TOOL.into());
        }
        let Some((plugin_id, tool, kind)) = plugin_tool_route(registry, name, &in_scope)? else {
            return Ok(UNKNOWN_TOOL.into());
        };
        Ok(
            match plugin_tool_entry(registry, running, &plugin_id, &tool) {
                ToolEntry::Found(_) if kind.is_some() => "execution-backed".into(),
                ToolEntry::Found(_) => "not delegable".into(),
                miss => miss
                    .miss_reason(&plugin_id, &tool)
                    .unwrap_or_else(|| "not delegable".into()),
            },
        )
    };
    let sanitized_hits = |pool: &[String], key: &str| -> Vec<String> {
        pool.iter()
            .filter(|candidate| codex_sanitized(candidate) == key)
            .cloned()
            .collect()
    };
    let eligible_list: Vec<String> = eligible.iter().cloned().collect();
    let visible_list: Vec<String> = visible.iter().cloned().collect();
    // A verbatim Codex spelling has no `plugin.` prefix; give an unresolved
    // one the registry shape so `validate_plugin_tools` lets the named
    // refusal below reach the Planner instead of a generic shape error.
    let kept = |name: &str| -> String {
        match name.strip_prefix("plugin_") {
            Some(rest) => format!("plugin.{rest}"),
            None => name.to_string(),
        }
    };
    let mut resolved = Vec::with_capacity(requested.len());
    let mut denied = std::collections::BTreeMap::new();
    for name in requested {
        if frozen.contains(name) || visible.contains(name) {
            if !eligible.contains(name) {
                denied.insert(name.clone(), format!("{name} ({})", reason(name)?));
            }
            resolved.push(name.clone());
            continue;
        }
        let key = codex_sanitized(name);
        let mut hits = sanitized_hits(frozen, &key);
        if hits.is_empty() {
            hits = sanitized_hits(&eligible_list, &key);
        }
        match hits.as_slice() {
            [real] => {
                if !eligible.contains(real) {
                    denied.insert(
                        real.clone(),
                        format!("{name} (resolves to {real}: {})", reason(real)?),
                    );
                }
                resolved.push(real.clone());
            }
            [] => match sanitized_hits(&visible_list, &key).as_slice() {
                [real] => {
                    denied.insert(
                        real.clone(),
                        format!("{name} (resolves to {real}: {})", reason(real)?),
                    );
                    resolved.push(real.clone());
                }
                [] => {
                    let kept = kept(name);
                    denied.insert(kept.clone(), format!("{name} ({UNKNOWN_TOOL})"));
                    resolved.push(kept);
                }
                several => {
                    let mut parts = Vec::with_capacity(several.len());
                    for real in several {
                        parts.push(format!("{real}: {}", reason(real)?));
                    }
                    let kept = kept(name);
                    denied.insert(
                        kept.clone(),
                        format!("{name} (matches {})", parts.join("; ")),
                    );
                    resolved.push(kept);
                }
            },
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

    fn strings(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn resolve(
        running_ids: &[&str],
        scope: TrackPluginScope,
        frozen: &[&str],
        requested: &[&str],
    ) -> Result<(Vec<String>, PluginToolAdmission), RpcError> {
        resolve_dispatch_plugin_tools_from(
            &registry(),
            &running(running_ids),
            &scope,
            &strings(frozen),
            &strings(requested),
        )
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
            &[],
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
            &[],
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
            let error = resolve(ALL, TrackPluginScope::All, &[], &[spelling]).unwrap_err();
            assert_eq!(error.code, RpcError::INVALID_PARAMS, "{error}");
            assert!(error.message.contains(spelling), "{error}");
            assert!(error.message.contains(DOTTED), "{error}");
            assert!(error.message.contains(COLLIDING), "{error}");
        }
        // Once only one of the pair is delegable the spelling is unique again.
        let (resolved, admission) = resolve(
            &["dev", TRUSTED_ID],
            TrackPluginScope::All,
            &[],
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
            &[],
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
        // Execution-backed is reported for a visible tool.
        let (resolved, admission) =
            resolve(ALL, TrackPluginScope::All, &[], &[FORGE_ACTION]).unwrap();
        let refusal = admission.refusal(&resolved).expect("refused");
        assert_eq!(
            refusal,
            format!("plugin_tools not delegable: {FORGE_ACTION} (execution-backed)")
        );
        // Both stopped: the collision cannot resolve, and the refusal says why for each.
        let (resolved, admission) = resolve(
            &[TRUSTED_ID],
            TrackPluginScope::All,
            &[],
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
        // Names outside the registry are unknown, whatever their shape.
        let (resolved, admission) = resolve(
            ALL,
            TrackPluginScope::All,
            &[],
            &["plugin.nope_x", "calm_report_read"],
        )
        .unwrap();
        assert_eq!(resolved, strings(&["calm_report_read", "plugin.nope_x"]));
        assert_eq!(
            admission.refusal(&resolved).expect("refused"),
            format!(
                "plugin_tools not delegable: calm_report_read ({UNKNOWN_TOOL}), \
                 plugin.nope_x ({UNKNOWN_TOOL})"
            )
        );
    }

    /// #891 non-disclosure through dispatch: on a Track bound to `dev`, a
    /// probe for another plugin's tool — exact, `plugin.`-spelled or
    /// verbatim — reads exactly like a probe for a tool that does not exist,
    /// and an out-of-scope collider never turns an in-scope match ambiguous.
    #[test]
    fn bound_track_cannot_distinguish_out_of_scope_from_unknown() {
        let scope = TrackPluginScope::Only("dev".into());
        let probes = [
            TRUSTED,
            FORGE_ACTION,
            "plugin.dev_neige_git_forge_wf_tool",
            "plugin_dev_neige_git_forge_wf_tool",
            "plugin.zzz_nope",
            "plugin_zzz_nope",
        ];
        for probe in probes {
            let (resolved, admission) = resolve(ALL, scope.clone(), &[], &[probe]).unwrap();
            let expected = match probe.strip_prefix("plugin_") {
                Some(rest) => format!("plugin.{rest}"),
                None => probe.to_string(),
            };
            assert_eq!(resolved, vec![expected], "{probe}");
            assert_eq!(
                admission.refusal(&resolved).expect("refused"),
                format!("plugin_tools not delegable: {probe} ({UNKNOWN_TOOL})"),
                "{probe}"
            );
        }
        // The out-of-scope `DOTTED` is just another unknown string here: like
        // the verbatim spelling it sanitizes to the in-scope tool and resolves
        // there — refusing it instead would itself be an existence oracle.
        for probe in ["plugin_dev_echo_do_thing", DOTTED] {
            let (resolved, admission) = resolve(ALL, scope.clone(), &[], &[probe]).unwrap();
            assert_eq!(resolved, vec![COLLIDING.to_string()], "{probe}");
            assert!(admission.refusal(&resolved).is_none(), "{probe}");
        }
        // `TrackPluginScope::None` sees nothing at all.
        let (resolved, admission) =
            resolve(ALL, TrackPluginScope::None, &[], &[COLLIDING, TRUSTED]).unwrap();
        assert_eq!(
            admission.refusal(&resolved).expect("refused"),
            format!(
                "plugin_tools not delegable: {TRUSTED} ({UNKNOWN_TOOL}), {COLLIDING} ({UNKNOWN_TOOL})"
            )
        );
    }

    /// A receipt's frozen names win over live candidates: after `dev.echo`
    /// stops, the sanitized replay of a dispatch that froze its tool must
    /// still resolve to that tool, not to the delegable collider.
    #[test]
    fn frozen_names_win_over_live_candidates_for_sanitized_replay() {
        let (resolved, admission) = resolve(
            &["dev", TRUSTED_ID],
            TrackPluginScope::All,
            &[DOTTED],
            &["plugin_dev_echo_do_thing"],
        )
        .unwrap();
        assert_eq!(resolved, vec![DOTTED.to_string()]);
        assert_eq!(
            admission.refusal(&resolved).expect("refused"),
            format!(
                "plugin_tools not delegable: plugin_dev_echo_do_thing \
                 (resolves to {DOTTED}: plugin dev.echo is not running)"
            )
        );
        // Without the receipt the same spelling resolves live.
        let (resolved, _) = resolve(
            &["dev", TRUSTED_ID],
            TrackPluginScope::All,
            &[],
            &["plugin_dev_echo_do_thing"],
        )
        .unwrap();
        assert_eq!(resolved, vec![COLLIDING.to_string()]);
        // A frozen name that left the registry stays as written.
        let (resolved, admission) = resolve(
            ALL,
            TrackPluginScope::All,
            &["plugin.gone_x"],
            &["plugin.gone_x"],
        )
        .unwrap();
        assert_eq!(resolved, vec!["plugin.gone_x".to_string()]);
        assert_eq!(
            admission.refusal(&resolved).expect("refused"),
            format!("plugin_tools not delegable: plugin.gone_x ({UNKNOWN_TOOL})")
        );
    }

    /// An unresolved verbatim spelling gets the `plugin.` shape so the
    /// contract validator lets the named refusal through.
    #[test]
    fn verbatim_unresolved_spelling_gets_registry_shape() {
        let (resolved, admission) = resolve(
            ALL,
            TrackPluginScope::All,
            &[],
            &["plugin_dev_missing_tool"],
        )
        .unwrap();
        assert_eq!(resolved, vec!["plugin.dev_missing_tool".to_string()]);
        calm_types::task_execution::validate_plugin_tools(&resolved).expect("registry shape");
        assert_eq!(
            admission.refusal(&resolved).expect("refused"),
            format!("plugin_tools not delegable: plugin_dev_missing_tool ({UNKNOWN_TOOL})")
        );
    }
}
