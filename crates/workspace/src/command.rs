use std::process::Command;

/// Git environment variables removed before every spawn: they redirect the repository, poison the commit
/// identity, or (`GIT_TEMPLATE_DIR`, `GIT_CONFIG*`) outrank the `-c` overrides and inject hooks.
const HOSTILE_GIT_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_INDEX_VERSION",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_CEILING_DIRECTORIES",
    // Discovery from a `-C <worktree>`: the lease module's checks read the
    // worktree git found from that directory.
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    "GIT_PREFIX",
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_AUTHOR_DATE",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "GIT_COMMITTER_DATE",
    // These outrank the `-c` overrides.
    "GIT_TEMPLATE_DIR",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG",
    "GIT_CONFIG_COUNT",
];

/// A `git` command with the hostile ambient variables removed; every git spawn on this path that is
/// not an [`isolated_git_command`] must use it.
pub fn neige_git_command() -> Command {
    let mut command = Command::new("git");
    for key in HOSTILE_GIT_ENV {
        command.env_remove(key);
    }
    command
}

/// A `git` command whose environment is an allowlist, not the server's, for every git run that
/// executes repository-selected code (hooks, smudge/clean/process filters, merge drivers,
/// fsmonitor). Kept: the base allowlist (`PATH`, `HOME`), `XDG_CONFIG_HOME`
/// when set (git reads the user's global config there too, e.g. `safe.directory`), and the C
/// locale. Callers add what a run needs, such as a commit identity; the upstream fetch adds its
/// network variables.
///
/// No network credential or proxy variable is inherited, so a filter that needs the network (a
/// Git LFS smudge, a credentialed process filter) cannot fetch during lease provisioning's
/// checkout. Supported scope: the attached repositories in use have no LFS or network filters, and
/// `worktree add` does not recurse into submodules.
///
pub fn isolated_git_command() -> Command {
    let mut command = Command::new("git");
    command
        .env_clear()
        .envs(
            ["PATH", "HOME", "XDG_CONFIG_HOME"]
                .into_iter()
                .filter_map(|key| std::env::var_os(key).map(|value| (key, value))),
        )
        .envs([("LANG", "C"), ("LC_ALL", "C")]);
    command
}
