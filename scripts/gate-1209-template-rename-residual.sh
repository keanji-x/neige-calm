#!/usr/bin/env bash
# #1209 PR-2 acceptance B10, widened by #1268 — residual scan for the retired
# `workflow` vocabulary.
#
# WHY THIS EXISTS
#
# #1209 renamed `workflow_id` -> `template_id` / `workflow_input` ->
# `template_input`. That rename had three classes of call site. Two of them the
# compiler reports one by one. The third does not exist for the compiler at all:
# literal SQL column lists, `Omit<T, 'literal'>` type keys, string rosters in
# tests, oracle YAML, aria-labels, CSS comments. Three independent readers
# scanned for those sites during design review and each found ones the previous
# had missed, so no hand-written site list can be claimed complete.
#
# #1268 then removed the *rest* of the vocabulary — the plugin-manifest
# `workflows[]` key, `WorkflowDescriptor`, `HostError::WorkflowConflict`,
# `workflow_templates.rs` and its exports, `resolve_trusted_workflow`, the
# `## Bound Workflow Input` prompt heading. #1209 had deliberately spared the
# plugin side because renaming a parsed key of a Tier A third-party contract
# breaks every existing manifest at parse time; the user confirmed there are no
# third-party plugins, so that cost's payer is the empty set and the exemption
# is gone. This gate is what keeps it gone.
#
# This script is the actual guarantee: a whole-repo scan with an allowlist where
# EVERY entry states why it is there and pins how many lines it is allowed to
# match. A new stale occurrence anywhere — including inside an allowlisted file
# — fails the gate. An allowlist entry that has stopped matching also fails, so
# the list cannot rot into a fig leaf.
#
# ---------------------------------------------------------------------------
# WHAT THE PATTERN COVERS, STATED HONESTLY
# ---------------------------------------------------------------------------
#
# It matches `workflow` (any case) only when it is **adjacent to an identifier
# character** on one side or the other — i.e. when it is part of a name or a
# data key, not when it is the ordinary English word:
#
#   MUST FAIL (vocabulary):  workflow_id  workflows  WorkflowDescriptor
#                            WORKFLOW_TEMPLATES  bound_workflow  workflowSelect
#                            forge_workflow_e2e  workflowId
#   MUST PASS (not ours):    "the rebase workflow", "Operator workflow:",
#                            "a nested workflow must cancel explicitly",
#                            the `Workflow` <select> label in the legacy `web/`
#                            bundle (renaming that is a USER-VISIBLE change,
#                            out of #1268's mechanical-rename scope)
#
# The `.github/workflows/` directory is GitHub Actions' name, not ours, so a
# `workflow` preceded by `.github/` is excluded by a lookbehind rather than by
# allowlisting the ~14 files that cite that path. A `workflows/...` path written
# WITHOUT the `.github/` prefix still matches (see allowlist entry 12, which is
# a test asserting exactly that near-miss).
#
# Scope, stated honestly: this scans file CONTENT, not file names. Two paths
# still carry the old spelling in their names (`0059_waves_workflow_id.sql`,
# `0061_waves_workflow_input.sql`); renaming a released migration file is
# forbidden.
#
# Positive/negative pair (run these to prove the gate discriminates):
#   * put the old column name back in `routes/today.rs`'s UPDATE  => must FAIL
#     (that file is not on the allowlist, and nothing else would catch it —
#     it compiles clean and only breaks at runtime)
#   * rename `Manifest::templates` back to `workflows`            => must FAIL
#     (the #1268 half; `manifest.rs` IS allowlisted, so this exercises the
#     per-file line-count ratchet rather than the unknown-path branch)
#   * leave `0059_waves_workflow_id.sql` spelled the old way       => must PASS
#     (allowlist entry 1; editing an applied migration is forbidden)

set -uo pipefail

PATTERN='(?<!\.github/)(?:workflow[a-z0-9_]|[a-z0-9_]workflow)'

# ---------------------------------------------------------------------------
# Allowlist: "<expected line count> <path>  # reason"
#
# The count is the number of MATCHING LINES `git grep -c` reports for that path.
# ---------------------------------------------------------------------------
read -r -d '' ALLOWLIST <<'EOF' || true
# --- 1. Released migrations. sqlx checksums the whole file including comments,
#        so editing an applied migration bricks startup with VersionMismatch.
#        Each of these ran against the schema of its own point in history, which
#        is strictly before the rename; replay order is unchanged. 0076 also
#        reads `$.workflows` out of the stored plugin manifest — correct there,
#        because the rows it reads were written before #1268.
1   crates/calm-truth/migrations/0059_waves_workflow_id.sql
2   crates/calm-truth/migrations/0061_waves_workflow_input.sql
10  crates/calm-truth/migrations/0076_waves_plugin_scope.sql
# --- 2. The rename migration itself has to name what it renames.
5   crates/calm-truth/migrations/0079_waves_rename_workflow_id_to_template_id.sql
# --- 3. Migration fixtures pinned to a historical schema. These build rows
#        through a migrator truncated BEFORE the rename, so the old column names
#        (and, for 0076, the old manifest key) are the correct ones there;
#        renaming them would break the fixture.
13  crates/calm-truth/src/db/sqlite/wave_plugin_scope_migration_tests.rs
4   crates/calm-truth/src/db/sqlite/wave_template_rename_migration_tests.rs
# --- 4. #1268's own loud-failure guard. `Manifest` tolerates unknown top-level
#        keys, so a manifest still spelling the array `workflows` would parse
#        into `templates: []` and silently declare no binding. `Manifest::parse`
#        refuses it by name; the check and its two tests must spell the retired
#        key, because that string IS the thing being refused.
10  crates/calm-server/src/plugin_host/manifest.rs
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
11  crates/calm-server/tests/cases/wave_template_waves.rs
# --- 8. Explanatory comment on the Today-launchpad rename pins.
1   crates/calm-server/tests/cases/today_launchpad.rs
# --- 8b. The migration filename inventory has to spell the migration's name,
#         and that file name records what the migration renames.
1   crates/calm-server/tests/cases/head_schema_fixture.rs
# --- 8c. Replay of a RETIRED event kind. `workflow.registered` /
#         `WorkflowRegistered` left the enum in #1110 S5, but rows carrying it
#         are immutable history; the test proves the reader skips them instead
#         of failing. Renaming the spelling would stop testing the real rows.
4   crates/calm-truth/tests/events_since_bound.rs
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
#         spellings by name (and #1268 added dated notes marking the plugin-side
#         half of that argument as overtaken, rather than deleting it);
#         `_1148-impl-report.md` is a frozen report of a past PR and rewriting
#         it would falsify the record.
394 docs/architecture/1209-template-workflow-unify.md
3   docs/_1148-impl-report.md
# --- 10b. The upgrade guide quotes, verbatim, the rejections an operator's
#          pre-rename script and pre-rename manifest will see, and ships a jq
#          scanner that has to look for the retired manifest key by name.
#          Paraphrasing them would make the doc less useful than the thing it
#          documents.
7   docs/deploy-and-upgrade.md
# --- 10c. Oracle record of a #891-era byte-identical-body assertion: the plain
#          `task` variant must send no `workflow_*` key at all. The claim is
#          about a wire shape that no longer exists, which is exactly why the
#          spelling has to stay.
1   docs/oracle/pages-shared.yaml
# --- 12. GitHub Actions, not us. This one line is a deliberate NEAR-MISS
#         fixture: `'workflows/ci.yml'` written WITHOUT the `.github/` prefix,
#         asserting the mutation runner does not treat it as the CI file. The
#         lookbehind that exempts real `.github/workflows/` paths must not
#         exempt this, or the fixture would stop being a near miss.
1   fe/tools/mutation/runner.test.ts
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
done < <(git grep -P -c -i "$PATTERN" -- . "$SELF_EXCLUDE" || true)

fail=0

for path in "${!ACTUAL[@]}"; do
  if [ -z "${EXPECTED[$path]+set}" ]; then
    echo "::error::residual workflow vocabulary: '$path' is not on the allowlist"
    git grep -P -n -i "$PATTERN" -- "$path"
    fail=1
  elif [ "${ACTUAL[$path]}" != "${EXPECTED[$path]}" ]; then
    echo "::error::residual workflow vocabulary: '$path' matches ${ACTUAL[$path]} line(s), allowlist says ${EXPECTED[$path]}"
    git grep -P -n -i "$PATTERN" -- "$path"
    fail=1
  fi
done

for path in "${!EXPECTED[@]}"; do
  if [ -z "${ACTUAL[$path]+set}" ]; then
    echo "::error::residual scan: allowlist entry '$path' no longer matches anything — delete the entry (a stale allowlist is a fig leaf)"
    fail=1
  fi
done

if [ "$fail" -ne 0 ]; then
  echo "::error::#1209 B10 / #1268 residual scan failed. Every allowlist entry must state why it is there; see the header of $0."
  exit 1
fi

echo "OK: no residual workflow vocabulary outside the ${#EXPECTED[@]}-entry allowlist (this is drift detection over a stated allowlist, not a proof that the site list was complete)"
