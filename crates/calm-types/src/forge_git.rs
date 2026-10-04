//! Shared git-forge shell scripts.
//!
//! These constants are consumed by both the kernel's automatic worker commit
//! path and the `git-forge` plugin binary. Keep them here so probe/recovery
//! semantics cannot drift between the two entry points.

/// Defines `neige_git` and `neige_gh` (#1830 S3 D4), the one credential split of every kernel
/// git script that runs repository-selected code (hooks, fsmonitor, filters, credential helpers,
/// `core.sshCommand`) or the network: git runs without a GitHub token (it keeps HOME, the
/// proxies and the ssh agent a push may need), and gh runs from `/`, outside any repository,
/// without the ssh keys. Prepended by the argv builders of the scripts that call them: every
/// script of the plugin's `git.commit` (action, probe and output probe: `status` runs
/// `core.fsmonitor` and clean filters, `log` runs `gpg.program` under `log.showSignature`), the
/// delivery action and the publish scripts. The delivery probes and the provenance function run
/// only `rev-parse`, `merge-base --is-ancestor` and `worktree list`, which run no repository
/// code, and stay bare.
pub const FORGE_SHELL_PRELUDE: &str = "neige_git() { env -u GH_TOKEN -u GITHUB_TOKEN -u GH_ENTERPRISE_TOKEN -u GITHUB_ENTERPRISE_TOKEN git \"$@\"; }\n\
    neige_gh() { (cd / && env -u SSH_AUTH_SOCK -u GIT_SSH_COMMAND gh \"$@\"); }";

/// The Planner commit's probe; runs after [`FORGE_SHELL_PRELUDE`] (its `status` runs repository
/// code).
pub const GIT_COMMIT_PROBE_SCRIPT: &str = "neige_git rev-parse --verify HEAD >/dev/null 2>&1 || exit 3; \
     status=$(neige_git status --porcelain) || exit 3; if [ -z \"$status\" ]; then exit 0; else exit 1; fi";

/// `$1 message, $2 branch?`; runs after [`FORGE_SHELL_PRELUDE`].
pub const GIT_COMMIT_SCRIPT: &str = r#"branch=${2:-$(neige_git rev-parse --abbrev-ref HEAD)} || exit 1; neige_git add -A || exit 1; if neige_git diff --cached --quiet; then :; else neige_git commit -m "$1" || exit 1; fi; json_escape() { awk 'BEGIN { s = ARGV[1]; ARGV[1] = ""; gsub(/\\/, "\\\\", s); gsub(/"/, "\\\"", s); gsub(/\n/, "\\n", s); gsub(/\t/, "\\t", s); gsub(/\r/, "\\r", s); printf "%s", s }' "$1"; }; commit=$(neige_git log -1 --format=%H) || exit 1; branch_json=$(json_escape "$branch") || exit 1; printf '{"commit":"%s","branch":"%s"}\n' "$commit" "$branch_json""#;

/// The Planner commit's output probe; runs after [`FORGE_SHELL_PRELUDE`] (its `log` can run
/// `gpg.program`).
pub const GIT_COMMIT_OUTPUT_PROBE_SCRIPT: &str = r#"branch=${1:-$(neige_git rev-parse --abbrev-ref HEAD)} || exit 1; json_escape() { awk 'BEGIN { s = ARGV[1]; ARGV[1] = ""; gsub(/\\/, "\\\\", s); gsub(/"/, "\\\"", s); gsub(/\n/, "\\n", s); gsub(/\t/, "\\t", s); gsub(/\r/, "\\r", s); printf "%s", s }' "$1"; }; commit=$(neige_git log -1 --format=%H) || exit 1; branch_json=$(json_escape "$branch") || exit 1; printf '{"commit":"%s","branch":"%s"}\n' "$commit" "$branch_json""#;

/// Defines `neige_lease_provenance <canonical_path> <git_common_dir>` (#1727 S4 D2 / D3.0): the one
/// text that checks a lease worktree's identity. Every git observation is status-checked before its
/// output is used; the registration test is a shell-builtin substring match (no pipe, no nested
/// substitution). The observation line goes to stderr always and to stdout only when the check does
/// not hold. Returns 0 (holds) / 10 (realpath or common dir differ) / 12 (identity holds but the
/// worktree is not registered) / 1 (an observation failed — not a verdict).
pub const GIT_LEASE_PROVENANCE_SCRIPT: &str = "neige_lease_provenance() {\n\
    rp=$(pwd -P) || return 1\n\
    gcd=$(git rev-parse --path-format=absolute --git-common-dir) || return 1\n\
    cd_=$(cd \"$gcd\" && pwd -P) || return 1\n\
    wl=$(git worktree list --porcelain) || return 1\n\
    reg=0; nl='\n\
    '; case \"$nl$wl$nl\" in *\"${nl}worktree $1${nl}\"*) reg=1;; esac\n\
    printf 'provenance realpath=%s common_dir=%s registered=%s\\n' \"$rp\" \"$cd_\" \"$reg\" >&2\n\
    [ \"$rp\" = \"$1\" ] && [ \"$cd_\" = \"$2\" ] && [ \"$reg\" = 1 ] && return 0\n\
    printf 'provenance realpath=%s common_dir=%s registered=%s\\n' \"$rp\" \"$cd_\" \"$reg\"\n\
    [ \"$rp\" = \"$1\" ] && [ \"$cd_\" = \"$2\" ] && return 12\n\
    return 10\n\
    }";

/// The kernel delivery script (#1727 S4 D2): `$1 message, $2 branch, $3 ref, $4 base_sha,
/// $5 canonical_path, $6 git_common_dir`; run as
/// `sh -c "<FORGE_SHELL_PRELUDE>\n<GIT_LEASE_PROVENANCE_SCRIPT>\n<this>"`.
/// Order: provenance, operation in progress, branch, base object, stage/commit, ancestry, ref —
/// the in-progress check runs before the branch check because a conflicted rebase detaches HEAD.
/// Exit vocabulary: 0 printed the JSON line and created the ref; 10 / 11 / 12 / 15 are provenance
/// mismatches (identity, branch, unregistered, operation in progress — 15 prints its evidence
/// first); 13 the base object is unreadable or `merge-base` failed; 14 an observation failed;
/// anything else is git's own code. Every non-zero exit happens before `update-ref`, which is the
/// last change the script makes; the ancestry observation is computed on the same OID the ref pins.
/// No observation goes through ref DWIM: the branch check compares the full symbolic ref
/// (`refs/heads/<branch>`; a tag named like the branch makes `--short` print `heads/<branch>`),
/// and the in-progress check tests the worktree-private pseudo-ref *files* (`--git-path`; an
/// ordinary branch named `MERGE_HEAD` resolves under `rev-parse --verify`) plus the two state
/// *directories* `rebase-merge` / `rebase-apply` (`[ -e ]` holds for either): an interactive
/// rebase paused at `break` and a `git am` whose conflict was `git add`ed but not `--continue`d
/// leave no pseudo-ref at all — the first detaches HEAD with no `REBASE_HEAD`, the second
/// leaves HEAD on the branch with a clean index. The evidence printed for a directory is its name.
pub const GIT_DELIVERY_SCRIPT: &str = "set -e\n\
    rc=0; neige_lease_provenance \"$5\" \"$6\" || rc=$?\n\
    case $rc in 0) ;; 10|12) exit $rc;; *) exit 14;; esac\n\
    u=$(neige_git ls-files -u) || exit 14\n\
    [ -z \"$u\" ] || { printf '%s\\n' \"$u\"; exit 15; }\n\
    for h in MERGE_HEAD CHERRY_PICK_HEAD REVERT_HEAD REBASE_HEAD rebase-merge rebase-apply; do\n\
    p=$(neige_git rev-parse --git-path \"$h\") || exit 14\n\
    [ -e \"$p\" ] || continue\n\
    printf '%s\\n' \"$h\"; exit 15\n\
    done\n\
    rc=0; ref_out=$(neige_git symbolic-ref -q HEAD) || rc=$?\n\
    case $rc in 0) ref=$ref_out;; 1) ref='';; *) exit 14;; esac\n\
    [ \"$ref\" = \"refs/heads/$2\" ] || exit 11\n\
    neige_git cat-file -e \"$4^{commit}\" || exit 13\n\
    neige_git add -A\n\
    neige_git diff --cached --quiet || neige_git commit -q -m \"$1\"\n\
    new=$(neige_git rev-parse --verify HEAD^{commit})\n\
    rc=0; neige_git merge-base --is-ancestor \"$4\" \"$new\" || rc=$?\n\
    case $rc in 0) anc=true;; 1) anc=false;; *) exit 13;; esac\n\
    neige_git update-ref \"$3\" \"$new\"\n\
    printf '{\"commit\":\"%s\",\"branch\":\"%s\",\"delivery_id\":\"%s\",\"base_is_ancestor\":%s}\\n' \
    \"$new\" \"$2\" \"${3##*/}\" \"$anc\"";

/// `$1 ref`: exit 0 = the candidate ref exists (landed), 1 = it does not (not landed), anything
/// else = unknown. Reads neither HEAD nor the working tree.
pub const GIT_DELIVERY_PROBE_SCRIPT: &str = "git rev-parse --verify -q \"$1^{commit}\" >/dev/null";

/// `$1 branch, $2 ref, $3 base_sha`: re-prints the delivery script's JSON line from the ref it
/// pinned — the same ancestry observation and `printf` as `GIT_DELIVERY_SCRIPT`, with no
/// `update-ref` in between — so the live stdout and the recovered stdout are byte-equal.
pub const GIT_DELIVERY_OUTPUT_PROBE_SCRIPT: &str = "set -e\n\
    new=$(git rev-parse --verify \"$2^{commit}\")\n\
    rc=0; git merge-base --is-ancestor \"$3\" \"$new\" || rc=$?\n\
    case $rc in 0) anc=true;; 1) anc=false;; *) exit 13;; esac\n\
    printf '{\"commit\":\"%s\",\"branch\":\"%s\",\"delivery_id\":\"%s\",\"base_is_ancestor\":%s}\\n' \
    \"$new\" \"$1\" \"${2##*/}\" \"$anc\"";
