use calm_server::worker_flow::claude_transcript::slug_for_projects;

#[test]
fn claude_project_slug_preserves_verified_ascii_allowlist() {
    assert_eq!(slug_for_projects("/home/kenji"), "-home-kenji");
    assert_eq!(
        slug_for_projects("/home/kenji/.codex"),
        "-home-kenji-.codex"
    );
    assert_eq!(
        slug_for_projects("/home/kenji/galxe/external/gravity_core"),
        "-home-kenji-galxe-external-gravity_core"
    );
    assert_eq!(
        slug_for_projects("/home/kenji/Abyssal/.claude/worktrees/cuddly-tickling-puzzle"),
        "-home-kenji-Abyssal-.claude-worktrees-cuddly-tickling-puzzle"
    );
    assert_eq!(slug_for_projects(""), "");
    assert_eq!(slug_for_projects("/tmp/a b"), "-tmp-a-b");
}

#[test]
fn claude_project_slug_replaces_non_ascii_per_character() {
    assert_eq!(slug_for_projects("/tmp/é"), "-tmp--");
    assert_eq!(slug_for_projects("/home/user/中文"), "-home-user---");
}
