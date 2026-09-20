#!/usr/bin/env bash
# Residual scan for the retired `workflow` vocabulary: a whole-repo grep with an
# allowlist where every entry states why it is there and pins how many lines it
# may match; an entry that stops matching fails too.
# The plural `workflows` always matches (the trailing `s` is an identifier char);
# genuine-English prose goes on the allowlist rather than being reworded.

set -uo pipefail

PATTERN='(?<!\.github/)(?:workflow[a-z0-9_]|[a-z0-9_]workflow)|Bound Workflow Input|Workflow input:'

# Allowlist: "<expected line count> <path>" — the count is the number of matching lines `git grep -c` reports for that path.
read -r -d '' ALLOWLIST <<'EOF' || true
# --- 1. Released migrations: sqlx checksums the whole file, so editing an applied migration bricks startup with VersionMismatch.
1   crates/calm-truth/migrations/0059_waves_workflow_id.sql
2   crates/calm-truth/migrations/0061_waves_workflow_input.sql
10  crates/calm-truth/migrations/0076_waves_plugin_scope.sql
# --- 2. The rename migration itself has to name what it renames.
5   crates/calm-truth/migrations/0079_waves_rename_workflow_id_to_template_id.sql
# --- 2b. The area-rename migration cites 0079 as its precedent by the exact column names it renamed.
1   crates/calm-truth/migrations/0080_cove_to_area.sql
# --- 3. Migration fixtures built through a migrator truncated before the rename; the old column names are correct there.
11  crates/calm-truth/src/db/sqlite/track_plugin_scope_migration_tests.rs
4   crates/calm-truth/src/db/sqlite/track_template_rename_migration_tests.rs
# --- 4. `Manifest::parse` refuses the retired `workflows` key by name; the check, its tests, and the field doc must spell it.
9  crates/calm-server/src/plugin_host/manifest.rs
# --- 5. The deserialize-only `#[serde(alias)]` that lets historical `track.updated` rows replay; deleting it is the fail-open this gate prevents.
2   crates/calm-types/src/model.rs
# --- 6. Goldens pinning that alias: their `wire` half is the OLD spelling on purpose, proving the alias is one-way.
2   crates/calm-server/tests/goldens/events/track_updated.legacy_template_id.json
2   crates/calm-server/tests/goldens/events/track_updated.legacy_template_input.json
1   crates/calm-server/tests/cases/event_serde_goldens.rs
# --- 7. Tests pinning the REJECTION of the old spelling on the write side; they must send the old keys.
9  crates/calm-server/tests/cases/track_template_tracks.rs
# --- 8b. The migration filename inventory has to spell the migration's name.
1   crates/calm-server/tests/cases/head_schema_fixture.rs
# --- 8c. Replay of the retired `workflow.registered` event kind: rows carrying it are immutable history and the reader must skip them.
3   crates/calm-truth/tests/events_since_bound.rs
# --- 9. The three zod readers' one-way normalize and their tests; each reader holds its own copy on purpose.
3   fe/core/api/schemas.ts
6   fe/core/api/schemas.contract.test.ts
# --- 10. Design + historical records that argue about both spellings by name; rewriting them would falsify the record.
394 docs/architecture/1209-template-workflow-unify.md
3   docs/_1148-impl-report.md
# --- 10b. The upgrade guide quotes verbatim the rejections an operator will see and ships a jq scanner for the retired key.
7   docs/deploy-and-upgrade.md
# --- 10c. Oracle record asserting the plain `task` variant sends no `workflow_*` key at all.
1   docs/archive/legacy-web-oracle/pages-shared.yaml
# --- 12. Deliberate NEAR-MISS fixture: `'workflows/ci.yml'` without the `.github/` prefix; the lookbehind must not exempt it.
1   fe/tools/mutation/runner.test.ts
# --- 14. GitHub Actions' `workflow_dispatch` event name is a platform-owned key, not the retired vocabulary.
3   .github/workflows/ci.yml
EOF

# This script names the pattern and quotes allowlisted paths in its own reasons, so it is excluded from its own scan.
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
