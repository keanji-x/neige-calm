//! Shared git-forge shell scripts.
//!
//! These constants are consumed by both the kernel's automatic worker commit
//! path and the `git-forge` plugin binary. Keep them here so probe/recovery
//! semantics cannot drift between the two entry points.

pub const GIT_COMMIT_PROBE_SCRIPT: &str = "git rev-parse --verify HEAD >/dev/null 2>&1 || exit 3; \
     status=$(git status --porcelain) || exit 3; if [ -z \"$status\" ]; then exit 0; else exit 1; fi";

pub const GIT_COMMIT_SCRIPT: &str = r#"branch=${2:-$(git rev-parse --abbrev-ref HEAD)} || exit 1; git add -A || exit 1; if git diff --cached --quiet; then :; else git commit -m "$1" || exit 1; fi; json_escape() { awk 'BEGIN { s = ARGV[1]; ARGV[1] = ""; gsub(/\\/, "\\\\", s); gsub(/"/, "\\\"", s); gsub(/\n/, "\\n", s); gsub(/\t/, "\\t", s); gsub(/\r/, "\\r", s); printf "%s", s }' "$1"; }; commit=$(git log -1 --format=%H) || exit 1; branch_json=$(json_escape "$branch") || exit 1; printf '{"commit":"%s","branch":"%s"}\n' "$commit" "$branch_json""#;

pub const GIT_COMMIT_OUTPUT_PROBE_SCRIPT: &str = r#"branch=${1:-$(git rev-parse --abbrev-ref HEAD)} || exit 1; json_escape() { awk 'BEGIN { s = ARGV[1]; ARGV[1] = ""; gsub(/\\/, "\\\\", s); gsub(/"/, "\\\"", s); gsub(/\n/, "\\n", s); gsub(/\t/, "\\t", s); gsub(/\r/, "\\r", s); printf "%s", s }' "$1"; }; commit=$(git log -1 --format=%H) || exit 1; branch_json=$(json_escape "$branch") || exit 1; printf '{"commit":"%s","branch":"%s"}\n' "$commit" "$branch_json""#;
