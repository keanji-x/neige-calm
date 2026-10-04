/// The track publish (#1830 S3 D5): `$1 sha, $2 branch, $3 url, $4 base, $5 title, $6 body`; runs
/// after [`calm_types::forge_git::FORGE_SHELL_PRELUDE`] in the track worktree. Pushes exactly `$1` (and its ancestors) to
/// `refs/heads/$2` at `$3`, never forced; opens the PR unless an open one already has head `$2`;
/// then prints the open PR's `{number, headRefOid, url}` — the only stdout — and exits 21 unless its
/// `headRefOid` is `$1`, re-reading it up to five times two seconds apart (GitHub moves an open
/// PR's head asynchronously after a push). `$3` is both the push destination and gh's `--repo`.
pub(super) const PR_PUBLISH_SCRIPT: &str = r#"neige_git push --porcelain "$3" "$1:refs/heads/$2" >&2 || exit $?
st=$(neige_gh pr view "$2" --repo "$3" --json state) || st=
case "$st" in *'"OPEN"'*) ;; *) neige_gh pr create --repo "$3" --head "$2" --base "$4" --title "$5" --body "$6" >&2 || exit $?;; esac
i=0
while :; do
pr=$(neige_gh pr view "$2" --repo "$3" --json number,headRefOid,url) || exit $?
flat=$(printf '%s' "$pr" | tr -d ' \t\r\n')
case "$flat" in *"\"headRefOid\":\"$1\""*) break;; esac
[ "$i" -lt 5 ] || { printf '%s\n' "$pr" >&2; exit 21; }
i=$((i + 1)); sleep 2
done
printf '%s\n' "$pr""#;

/// `$1 sha, $2 branch, $3 url`, after [`calm_types::forge_git::FORGE_SHELL_PRELUDE`]: exit 0 = landed (`$3` has
/// `refs/heads/$2` at `$1` and the open PR of `$2` has head `$1`), 1 = not landed, 3 = the remote
/// could not be read.
pub(super) const PR_PUBLISH_PROBE_SCRIPT: &str = r#"out=$(neige_git ls-remote "$3" "refs/heads/$2") || exit 3
[ "$out" = "$(printf '%s\trefs/heads/%s' "$1" "$2")" ] || exit 1
pr=$(neige_gh pr view "$2" --repo "$3" --json headRefOid,state) || exit 1
flat=$(printf '%s' "$pr" | tr -d ' \t\r\n')
case "$flat" in *'"state":"OPEN"'*) ;; *) exit 1;; esac
case "$flat" in *"\"headRefOid\":\"$1\""*) exit 0;; *) exit 1;; esac"#;

/// `$1 sha, $2 branch, $3 url`, after [`calm_types::forge_git::FORGE_SHELL_PRELUDE`]: the publish script's last line,
/// so the live stdout and the recovered stdout come from the same command.
pub(super) const PR_PUBLISH_OUTPUT_PROBE_SCRIPT: &str =
    r#"neige_gh pr view "$2" --repo "$3" --json number,headRefOid,url"#;
