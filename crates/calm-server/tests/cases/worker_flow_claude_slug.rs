use calm_server::worker_flow::claude_transcript::slug_for_projects;

/// Claude 2.1.280 keeps ASCII letters, digits and `-`; `.`, `_` and every other unit become `-`.
#[test]
fn claude_project_slug_preserves_verified_ascii_allowlist() {
    assert_eq!(slug_for_projects("/home/kenji"), "-home-kenji");
    assert_eq!(
        slug_for_projects("/home/kenji/.codex"),
        "-home-kenji--codex"
    );
    assert_eq!(
        slug_for_projects("/home/kenji/galxe/external/gravity_core"),
        "-home-kenji-galxe-external-gravity-core"
    );
    assert_eq!(
        slug_for_projects("/home/kenji/Abyssal/.claude/worktrees/cuddly-tickling-puzzle"),
        "-home-kenji-Abyssal--claude-worktrees-cuddly-tickling-puzzle"
    );
    assert_eq!(slug_for_projects(""), "");
    assert_eq!(slug_for_projects("/tmp/a b"), "-tmp-a-b");
    // Observed from the 2.1.280 CLI started in `.../ws_a.b-c+d é`.
    assert_eq!(slug_for_projects("/w/ws_a.b-c+d é"), "-w-ws-a-b-c-d--");
}

#[test]
fn claude_project_slug_replaces_bmp_non_ascii_per_code_unit() {
    assert_eq!(slug_for_projects("/tmp/é"), "-tmp--");
    assert_eq!(slug_for_projects("/home/user/中文"), "-home-user---");
}

#[test]
fn claude_project_slug_replaces_astral_chars_per_utf16_code_unit() {
    assert_eq!(slug_for_projects("/tmp/🎉"), "-tmp---");
    assert_eq!(slug_for_projects("/home/é/🎉/x"), "-home------x");
}
