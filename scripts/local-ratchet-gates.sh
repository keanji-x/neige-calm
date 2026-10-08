#!/usr/bin/env bash
# Run the CI lint job's text gates locally, so a change (including a docs-only one)
# that is green under local-rust-gates.sh is not red in CI.
# The gate list is read from ci.yml: every lint-job step whose whole command is
# `./scripts/gate-*.sh`. Not covered: the `--selftest` steps and the Rust-only steps.
# The ratchets count with `git grep --untracked`: they scan tracked and untracked files in the
# working tree, not HEAD, and skip ignored ones.
# Usage: scripts/local-ratchet-gates.sh

set -uo pipefail

root="$(git rev-parse --show-toplevel 2>/dev/null)" || {
  echo "error: not inside a git working tree; run from a neige-calm checkout" >&2
  exit 2
}
cd "$root" || exit 2

# Real probe, same pattern class as the prose ratchet: `\x{...}` needs PCRE2 in UTF mode.
probe_err="$(LC_ALL=C.UTF-8 git grep -P -q '[\x{4e00}-\x{9fff}]' 2>&1 >/dev/null)"
probe_status=$?
if [ "$probe_status" -ge 2 ]; then
  echo "error: \`git grep -P\` with Unicode patterns failed (exit $probe_status)." >&2
  echo "The ratchets need a git built with PCRE2 (Unicode support) and the C.UTF-8 locale." >&2
  echo "git said: $probe_err" >&2
  exit 2
fi

dirty_note() {
  [ -n "$(git --no-optional-locks status --porcelain --untracked-files=normal)" ] || return 0
  echo "note: the working tree is not clean. Results measure the working tree, not HEAD;"
  echo "      untracked files count too, so delete local scratch files or list them in .git/info/exclude."
}

dirty_note

ci=.github/workflows/ci.yml
mapfile -t gates < <(
  awk '/^  lint:$/ { in_lint = 1; next }
       in_lint && /^  [A-Za-z0-9_-]+:$/ { exit }
       in_lint && /^ +run: \.\/scripts\/gate-[A-Za-z0-9_.-]+\.sh *$/ { print $2 }' "$ci" 2>/dev/null
)
if [ "${#gates[@]}" -eq 0 ]; then
  echo "error: found no \`run: ./scripts/gate-*.sh\` step in the lint job of $ci" >&2
  exit 2
fi

declare -A result=()
for gate in "${gates[@]}"; do
  printf '\n=== %s\n' "$gate"
  if "$gate"; then result[$gate]=PASS; else result[$gate]=FAIL; fi
done

printf '\n=== summary\n'
failed=0
for gate in "${gates[@]}"; do
  printf '%s  %s\n' "${result[$gate]}" "$gate"
  [ "${result[$gate]}" = PASS ] || failed=1
done
dirty_note
exit "$failed"
