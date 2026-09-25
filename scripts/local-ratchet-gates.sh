#!/usr/bin/env bash
# Run the cross-tree text ratchets the way the CI lint job runs them, so a change
# (including a docs-only one) that is green under local-rust-gates.sh is not red in CI.
# Mirrors the ci.yml steps "terminology ratchet (#1316 S0)" and "prose ratchet (#1635 S6)".
# Not covered: the ratchets' `--selftest` steps and the other lint-job text gates.
# Counts come from `git grep`: they measure tracked files in the working tree, not HEAD.
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
  [ -n "$(git --no-optional-locks status --porcelain)" ] || return 0
  echo "note: the working tree is not clean. Results measure tracked files in the working tree, not HEAD;"
  echo "      untracked files are not counted (run \`git add -N <file>\` on new files you intend to commit)."
}

dirty_note

declare -A result=()
gates=(./scripts/gate-1316-terminology-ratchet.sh ./scripts/gate-prose-ratchet.sh)
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
