#!/usr/bin/env bash
# #1209 PR-2 acceptance B10 — residual scan for the pre-rename wave-create
# field spellings.
#
# WHY THIS EXISTS
#
# The rename `workflow_id` -> `template_id` / `workflow_input` ->
# `template_input` has three classes of call site. Two of them the compiler
# reports one by one. The third does not exist for the compiler at all: literal
# SQL column lists, `Omit<T, 'literal'>` type keys, string rosters in tests,
# oracle YAML, aria-labels, CSS comments. Three independent readers scanned for
# those sites during design review and each found ones the previous had missed,
# so no hand-written site list can be claimed complete.
#
# This script is the actual guarantee: a whole-repo scan with an allowlist where
# EVERY entry states why it is there and pins how many lines it is allowed to
# match. A new stale occurrence anywhere — including inside an allowlisted file
# — fails the gate. An allowlist entry that has stopped matching also fails, so
# the list cannot rot into a fig leaf.
#
# Scope, stated honestly: this scans file CONTENT, not file names. Two paths
# still carry the old spelling in their names for the reasons given below
# (`0059_waves_workflow_id.sql`, `0061_waves_workflow_input.sql`); renaming a
# released migration file is forbidden.
#
# Positive/negative pair (run these to prove the gate discriminates):
#   * put the old column name back in `routes/today.rs`'s UPDATE  => must FAIL
#     (that file is not on the allowlist, and nothing else would catch it —
#     it compiles clean and only breaks at runtime)
#   * leave `0059_waves_workflow_id.sql` spelled the old way       => must PASS
#     (allowlist entry 1; editing an applied migration is forbidden)

set -uo pipefail

PATTERN='workflow_id|workflow_input'

# ---------------------------------------------------------------------------
# Allowlist: "<expected line count> <path>  # reason"
#
# The count is the number of MATCHING LINES `git grep -c` reports for that path.
# ---------------------------------------------------------------------------
read -r -d '' ALLOWLIST <<'EOF' || true
# --- 1. Released migrations. sqlx checksums the whole file including comments,
#        so editing an applied migration bricks startup with VersionMismatch.
#        Each of these ran against the schema of its own point in history, which
#        is strictly before the rename; replay order is unchanged.
1   crates/calm-truth/migrations/0059_waves_workflow_id.sql
2   crates/calm-truth/migrations/0061_waves_workflow_input.sql
6   crates/calm-truth/migrations/0076_waves_plugin_scope.sql
# --- 2. The rename migration itself has to name what it renames.
5   crates/calm-truth/migrations/0079_waves_rename_workflow_id_to_template_id.sql
# --- 3. Migration fixtures pinned to a historical schema. These build rows
#        through a migrator truncated BEFORE the rename, so the old column names
#        are the correct ones there; renaming them would break the fixture.
5   crates/calm-truth/src/db/sqlite/wave_plugin_scope_migration_tests.rs
4   crates/calm-truth/src/db/sqlite/wave_template_rename_migration_tests.rs
# --- 4. Plugin-side vocabulary. A plugin manifest's `workflows[]` array is NOT
#        renamed: that key is a parsed field of a public third-party contract
#        (docs/upgrade-stability.md Tier A), and renaming it would break every
#        existing manifest at PARSE time — the one contract break #1209 avoids.
#        `HostError::WorkflowConflict.workflow_id` and the conflict tests carry
#        the id a plugin declared in that array, not the kernel request field.
2   crates/calm-server/src/plugin_host/error.rs
1   crates/calm-server/src/plugin_host/lifecycle.rs
6   crates/calm-server/src/plugin_host/mod.rs
5   crates/calm-server/tests/plugin_workflow_uniqueness.rs
# --- 5. The compatibility read itself. `calm_types::Wave` carries a
#        deserialize-only `#[serde(alias)]` so historical `wave.updated` rows —
#        which are immutable history — still replay with their template
#        attribution. Deleting these two lines is the fail-open this whole
#        slice was designed to prevent.
2   crates/calm-types/src/model.rs
# --- 6. The goldens that pin that alias, plus the comment that explains them.
#        Their `wire` half is deliberately the OLD spelling and their
#        `canonical` half the new one; that split is what proves the alias is
#        one-way.
2   crates/calm-server/tests/goldens/events/wave_updated.legacy_template_id.json
2   crates/calm-server/tests/goldens/events/wave_updated.legacy_template_input.json
1   crates/calm-server/tests/cases/event_serde_goldens.rs
# --- 7. The tests that pin the REJECTION of the old spelling on the write side.
#        They must send the old keys; that is the whole assertion.
11  crates/calm-server/tests/cases/wave_workflow_templates.rs
# --- 8. Explanatory comment on the Today-launchpad rename pins.
1   crates/calm-server/tests/cases/today_launchpad.rs
# --- 8b. The migration filename inventory has to spell the migration's name,
#         and that file name records what the migration renames.
1   crates/calm-server/tests/cases/head_schema_fixture.rs
# --- 9. The three zod readers' one-way normalize, and their tests. Each reader
#        holds its OWN copy on purpose: a shared helper would make "only the
#        third reader was missed" a green regression.
4   fe/core/api/schemas.ts
6   fe/core/api/schemas.contract.test.ts
4   web/src/api/schemas.ts
5   web/src/api/schemas.test.ts
3   web/src/wave-fs-viewers/schemas.ts
8   web/src/wave-fs-viewers/schemas.test.ts
# --- 10. Design + historical records. The #1209 design doc argues about both
#         spellings by name; `_1148-impl-report.md` is a frozen report of a past
#         PR and rewriting it would falsify the record.
179 docs/architecture/1209-template-workflow-unify.md
2   docs/_1148-impl-report.md
# --- 10b. The upgrade guide quotes, verbatim, the rejection an operator's
#          pre-rename script will see. Paraphrasing it would make the doc less
#          useful than the thing it documents.
1   docs/deploy-and-upgrade.md
EOF

# This script names the pattern it scans for and quotes several allowlisted
# paths in its own reasons, so it is excluded from its own scan rather than
# given a line count that would churn on every comment edit.
SELF_EXCLUDE=':!scripts/gate-1209-template-rename-residual.sh'

cd "$(git rev-parse --show-toplevel)" || exit 1

declare -A EXPECTED=()
while read -r count path _rest; do
  case "$count" in ''|'#'*) continue ;; esac
  EXPECTED["$path"]="$count"
done <<<"$ALLOWLIST"

declare -A ACTUAL=()
while IFS=: read -r path count; do
  [ -n "$path" ] || continue
  ACTUAL["$path"]="$count"
done < <(git grep -c -E "$PATTERN" -- . "$SELF_EXCLUDE" || true)

fail=0

for path in "${!ACTUAL[@]}"; do
  if [ -z "${EXPECTED[$path]+set}" ]; then
    echo "::error::#1209 residual: '$path' still carries the pre-rename spelling and is not on the allowlist"
    git grep -n -E "$PATTERN" -- "$path"
    fail=1
  elif [ "${ACTUAL[$path]}" != "${EXPECTED[$path]}" ]; then
    echo "::error::#1209 residual: '$path' matches ${ACTUAL[$path]} line(s), allowlist says ${EXPECTED[$path]}"
    git grep -n -E "$PATTERN" -- "$path"
    fail=1
  fi
done

for path in "${!EXPECTED[@]}"; do
  if [ -z "${ACTUAL[$path]+set}" ]; then
    echo "::error::#1209 residual: allowlist entry '$path' no longer matches anything — delete the entry (a stale allowlist is a fig leaf)"
    fail=1
  fi
done

if [ "$fail" -ne 0 ]; then
  echo "::error::#1209 PR-2 acceptance B10 failed. Every allowlist entry must state why it is there; see the header of $0."
  exit 1
fi

echo "OK: no residual pre-rename spelling outside the ${#EXPECTED[@]}-entry allowlist (this is drift detection over a stated allowlist, not a proof that the site list was complete)"
