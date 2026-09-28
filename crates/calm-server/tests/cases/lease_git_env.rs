//! #1792: the kernel's `git worktree add` runs the attached repository's own code (the
//! `post-checkout` and `reference-transaction` hooks here; smudge/process filters and fsmonitor
//! likewise), so it runs with an allowlisted environment, never the kernel's. Since #1830 S2 the
//! one worktree the kernel adds for a track's workers is the track worktree.
use super::git_delivery::*;

#[tokio::test]
async fn track_worktree_hooks_see_only_the_allowlisted_environment() {
    // SAFETY: nextest runs each test in its own process; nothing else reads the environment here.
    unsafe { std::env::set_var("NEIGE_LEASE_ENV_SENTINEL", "server-secret") };
    let fx = fixture_with(|tmp| {
        let repo = tmp.join("repo");
        init_repo(&repo);
        let hooks = repo.join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        for hook in ["post-checkout", "reference-transaction"] {
            let probe = tmp.join(format!("{hook}-env.txt"));
            write_executable(
                &hooks.join(hook),
                &format!("#!/bin/sh\nenv >> '{}'\n", probe.display()),
            );
        }
        repo
    })
    .await;
    let probes = fx.track_root.parent().unwrap().to_path_buf();

    assert!(fx.worktree.is_dir(), "the track worktree was made");
    for hook in ["post-checkout", "reference-transaction"] {
        let seen = std::fs::read_to_string(probes.join(format!("{hook}-env.txt")))
            .unwrap_or_else(|error| panic!("the {hook} hook ran: {error}"));
        assert!(seen.contains("PATH="), "{hook}: {seen}");
        assert!(seen.contains("HOME="), "{hook}: {seen}");
        assert!(
            !seen.contains("NEIGE_LEASE_ENV_SENTINEL"),
            "{hook} leaked: {seen}"
        );
    }
}
