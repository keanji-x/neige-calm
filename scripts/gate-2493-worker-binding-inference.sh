#!/usr/bin/env bash
# #2493 S1: which attempt a worker session serves is the explicit binding `tasks.worker_session_id`,
# owned by `crates/calm-truth/src/db/sqlite/worker_binding.rs`. This scan keeps the inferences it
# replaced out of the product code: card <-> attempt read through the spawn operation, the card
# stamp or the card payload. Operation <-> attempt by `idempotency_key` is the sanctioned
# convention (`operation/driver.rs`) and is not scanned.
# Scope: Rust sources under crates/ (released migrations are SQL and out of scope) outside test code (`tests/` directories, `*tests.rs`,
# `test_support*.rs`, `test_seams.rs`). `--untracked` scans tracked and untracked files in the
# working tree and skips ignored ones.

set -uo pipefail

cd "$(git rev-parse --show-toplevel)" || exit 2

# One pattern per retired inference (PCRE, one line each).
read -r -d '' PATTERNS <<'EOF' || true
spawn_op_id-read	(?:\.spawn_op_id\b(?!\s*:)|\bspawn_op_id\s*(?:=|IS\b)|=\s*\w+\.spawn_op_id\b)
op-target-proof	\bworker_op_targets\w*|\bWORKER_SPAWN_OPS\w*|\bowns_key\b
op-key-of-session	\boperation_idempotency_key_by_id\b
task-for-card	\btask_for_worker_card\b
card-payload-key	(?:\.payload\s*\.get\(\s*"idempotency_key"\s*\)|\.payload\s*\[\s*"idempotency_key"\s*\]|json_extract\(\s*(?:(?:c|cards)\.)?payload\s*,\s*'\$\.idempotency_key'\s*\))
card-stamp-filter	\bworker_card_id\s*(?:=\s*(?:\?\d*|[a-z]{1,3}\.\w+\b(?![.(]))|IS\s+(?:NOT\s+)?NULL)
lease-by-card	\bworkspace_leases\b[^;]*\bWHERE\s+(?:\w+\.)?card_id\s*=
EOF

# Exemptions: "<path> <reason>". No count pin: the path is out of scope for the reason given.
read -r -d '' EXEMPT <<'EOF' || true
crates/calm-truth/src/db/sqlite/worker_binding.rs	the owning module: its one writer stamps worker_card_id with the binding
crates/calm-truth/src/db/sqlite/session_mirror.rs	the session writer records spawn_op_id
crates/calm-truth/src/db/sqlite/session_row.rs	the session row writer and decoder carry spawn_op_id
crates/calm-truth/src/session_projection_row.rs	the session projection's SELECT list names spawn_op_id
EOF

declare -a exempt_paths=()
while IFS=$'\t' read -r path _reason; do
  [ -n "$path" ] && exempt_paths+=(":!$path")
done <<<"$EXEMPT"

scope=(
  'crates/**/*.rs'
  ':!crates/**/tests/**'
  ':!crates/**/*tests.rs'
  ':!crates/**/test_support*.rs'
  ':!crates/**/test_seams.rs'
  "${exempt_paths[@]}"
)

SELF='scripts/gate-2493-worker-binding-inference.sh'

if [ "${1:-}" = '--selftest' ]; then
  # Each pattern must fire on a product-code probe, and a probe in test code must not.
  probe='crates/calm-server/src/_gate_2493_selftest_probe.rs'
  test_probe='crates/calm-server/tests/_gate_2493_selftest_probe.rs'
  trap 'rm -f "$probe" "$test_probe"' EXIT
  cat >"$probe" <<'PROBE'
let op = session.spawn_op_id.as_deref();
let owns = worker_op_targets_card_tx(tx, a, c);
let key = repo.operation_idempotency_key_by_id(op);
let task = repo.task_for_worker_card(card);
let key = card.payload.get("idempotency_key");
"SELECT id FROM tasks WHERE worker_card_id = ?1"
"SELECT path FROM workspace_leases WHERE card_id = ?1"
PROBE
  output="$("./$SELF" 2>&1)" && { echo "SELFTEST FAIL: the probe passed the scan"; exit 1; }
  fails=0
  while IFS=$'\t' read -r name _pattern; do
    [ -n "$name" ] || continue
    case "$output" in
      *"FAIL $name:"*) echo "selftest ok: $name fires" ;;
      *) echo "SELFTEST FAIL: $name did not fire on its probe line"; fails=1 ;;
    esac
  done <<<"$PATTERNS"
  rm -f "$probe"
  echo 'let key = card.payload.get("idempotency_key");' >"$test_probe"
  "./$SELF" >/dev/null 2>&1 || { echo "SELFTEST FAIL: test code is scanned"; fails=1; }
  [ "$fails" -eq 0 ] && echo "selftest ok: test code is out of scope"
  exit "$fails"
fi

failed=0
while IFS=$'\t' read -r name pattern; do
  [ -n "$name" ] || continue
  hits="$(git grep --untracked -n -P -- "$pattern" "${scope[@]}" 2>&1)"
  status=$?
  if [ "$status" -ge 2 ]; then
    echo "error: git grep failed for $name: $hits" >&2
    exit 2
  fi
  if [ -n "$hits" ]; then
    echo "FAIL $name: read the binding through worker_binding.rs instead (#2493)"
    echo "$hits" | sed 's/^/  /'
    failed=1
  fi
done <<<"$PATTERNS"

if [ "$failed" -eq 0 ]; then
  echo "PASS gate-2493-worker-binding-inference: no card/attempt inference outside worker_binding.rs"
fi
exit "$failed"
