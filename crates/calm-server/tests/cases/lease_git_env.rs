//! #1792: lease provisioning's `git worktree add` runs the attached repository's own code (the
//! `post-checkout` and `reference-transaction` hooks here; smudge/process filters and fsmonitor
//! likewise), so it runs with an allowlisted environment, never the kernel's.
use calm_server::session_projection_repo::AgentProvider;

use super::git_delivery::*;

#[tokio::test]
async fn lease_provisioning_hooks_see_only_the_allowlisted_environment() {
    // SAFETY: nextest runs each test in its own process; nothing else reads the environment here.
    unsafe { std::env::set_var("NEIGE_LEASE_ENV_SENTINEL", "server-secret") };
    let fx = fixture().await;
    let probes = fx.track_root.parent().unwrap().to_path_buf();
    let hooks = fx.track_root.join(".git/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    for hook in ["post-checkout", "reference-transaction"] {
        let probe = probes.join(format!("{hook}-env.txt"));
        write_executable(
            &hooks.join(hook),
            &format!("#!/bin/sh\nenv >> '{}'\n", probe.display()),
        );
    }

    let worker = fx.new_worker("hooked", AgentProvider::Codex).await;
    let lease = fx.kernel_lease(&worker.card_id).await;

    assert!(lease.path.is_dir(), "the worktree was provisioned");
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
