/// The last line of [`PR_PUBLISH_SCRIPT`] and of [`PR_PUBLISH_OUTPUT_PROBE_SCRIPT`]: the gh
/// export `$pr` (`{number, headRefOid, url}`, compact or indented) with `"pr_action":"$how"`
/// spliced in front of its first field.
macro_rules! print_pr_line {
    () => {
        r#"printf '{"pr_action":"%s",%s\n' "$how" "${pr#*\{}""#
    };
}

/// The track publish (#1830 S3 D5): `$1 sha, $2 branch, $3 url, $4 base, $5 title, $6 body, $7 the
/// commits of this track's candidates (space-separated)`; runs after
/// [`calm_types::forge_git::FORGE_SHELL_PRELUDE`] in the track worktree. Reads the remote head of
/// `refs/heads/$2` first and exits 22, before any push or gh, unless it is absent, `$1`, or one of
/// `$7` (#2058 D1: a commit an attempt of this track made). When an open PR already has head `$2`,
/// gives it title `$5` and body `$6` first (`pr_action` `reused`, #2139), so a failed edit exits
/// before the push and the probe reports not landed. Then pushes exactly `$1` (and its ancestors)
/// there, leasing against that head (`--force-with-lease`), so it may replace the track's own head
/// but never a writer that landed since the read, and opens the PR with that title and body unless
/// it reused one (`created`). Then prints the open PR's `{pr_action, number, headRefOid, url}` —
/// the only stdout — and exits 21 unless its `headRefOid` is `$1`, re-reading it up to five times
/// two seconds apart (GitHub moves an open PR's head asynchronously after a push). `$3` is both
/// the push destination and gh's `--repo`.
pub(super) const PR_PUBLISH_SCRIPT: &str = concat!(
    r#"cur=$(neige_git ls-remote "$3" "refs/heads/$2") || exit $?; cur=${cur%%[[:space:]]*}
[ -z "$cur" ] || [ "$cur" = "$1" ] || case " $7 " in *" $cur "*) ;; *) exit 22;; esac
st=$(neige_gh pr view "$2" --repo "$3" --json state) || st=
how=created
case "$st" in *'"OPEN"'*) neige_gh pr edit "$2" --repo "$3" --title "$5" --body "$6" >&2 || exit $?; how=reused;; esac
neige_git push --porcelain --force-with-lease="refs/heads/$2:$cur" "$3" "$1:refs/heads/$2" >&2 || exit $?
[ "$how" = reused ] || neige_gh pr create --repo "$3" --head "$2" --base "$4" --title "$5" --body "$6" >&2 || exit $?
i=0
while :; do
pr=$(neige_gh pr view "$2" --repo "$3" --json number,headRefOid,url) || exit $?
flat=$(printf '%s' "$pr" | tr -d ' \t\r\n')
case "$flat" in *"\"headRefOid\":\"$1\""*) break;; esac
[ "$i" -lt 5 ] || { printf '%s\n' "$pr" >&2; exit 21; }
i=$((i + 1)); sleep 2
done
"#,
    print_pr_line!()
);

/// `$1 sha, $2 branch, $3 url`, after [`calm_types::forge_git::FORGE_SHELL_PRELUDE`]: exit 0 = landed (`$3` has
/// `refs/heads/$2` at `$1` and the open PR of `$2` has head `$1`), 1 = not landed, 3 = the remote
/// could not be read.
pub(super) const PR_PUBLISH_PROBE_SCRIPT: &str = r#"out=$(neige_git ls-remote "$3" "refs/heads/$2") || exit 3
[ "$out" = "$(printf '%s\trefs/heads/%s' "$1" "$2")" ] || exit 1
pr=$(neige_gh pr view "$2" --repo "$3" --json headRefOid,state) || exit 1
flat=$(printf '%s' "$pr" | tr -d ' \t\r\n')
case "$flat" in *'"state":"OPEN"'*) ;; *) exit 1;; esac
case "$flat" in *"\"headRefOid\":\"$1\""*) exit 0;; *) exit 1;; esac"#;

/// `$1 sha, $2 branch, $3 url`, after [`calm_types::forge_git::FORGE_SHELL_PRELUDE`]: the publish
/// script's read and last line, so the live stdout and the recovered stdout come from the same
/// commands. A recovered publish cannot tell whether it opened the PR or retitled an open one, so
/// its `pr_action` is `recovered`.
pub(super) const PR_PUBLISH_OUTPUT_PROBE_SCRIPT: &str = concat!(
    r#"how=recovered
pr=$(neige_gh pr view "$2" --repo "$3" --json number,headRefOid,url) || exit $?
"#,
    print_pr_line!()
);
