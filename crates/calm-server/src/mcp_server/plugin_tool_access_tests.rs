use super::*;

fn tool(assistant_access: bool, kind: Option<ToolKind>) -> ExposedTool {
    ExposedTool {
        name: "read".into(),
        assistant_access,
        kind,
        description: None,
        input_schema: None,
        annotations: None,
    }
}

#[test]
fn ordinary_local_tools_require_explicit_assistant_access() {
    assert!(
        !plugin_tool_roles(ConnectorKind::App, &tool(false, None)).contains(&CardRole::Assistant)
    );
    assert!(
        plugin_tool_roles(ConnectorKind::App, &tool(true, None)).contains(&CardRole::Assistant)
    );
}

#[test]
fn execution_and_connector_tools_never_gain_assistant_access() {
    // Defense in depth: even a malformed in-memory materialized entry does
    // not widen roles. Manifest parsing refuses these shapes independently.
    for (kind, tool_kind) in [
        (ConnectorKind::App, Some(ToolKind::ForgeAction)),
        (ConnectorKind::McpHttp, None),
        (ConnectorKind::CliQuery, None),
    ] {
        assert_eq!(
            plugin_tool_roles(kind, &tool(true, tool_kind)),
            PLUGIN_TOOL_ROLES
        );
    }
}

#[test]
fn assistant_plugin_access_requires_a_resolved_nonempty_track() {
    for track in [None, Some(""), Some(" \t")] {
        assert!(!plugin_role_has_track(CardRole::Assistant, track));
        assert!(plugin_role_has_track(CardRole::Planner, track));
        assert!(plugin_role_has_track(CardRole::Worker, track));
    }
    assert!(plugin_role_has_track(
        CardRole::Assistant,
        Some("track-current")
    ));
}
