#!/usr/bin/env bash

set -euo pipefail

script_dir="${BASH_SOURCE[0]%/*}"
[ "$script_dir" != "${BASH_SOURCE[0]}" ] || script_dir=.
script_dir="$(cd "$script_dir" && pwd)"
# shellcheck source=lib.sh
. "$script_dir/lib.sh"

require_tool rg

scan_root="${1:-$script_dir/../../..}"
require_path "$scan_root"
scan_root="$(cd "$scan_root" && pwd)"
require_path \
  "$scan_root/crates" \
  "$scan_root/crates/calm-truth/migrations"

cd "$scan_root"

failures=0

scan_must_be_empty \
  "runtimes table SQL (FROM/INTO/UPDATE/DELETE runtimes) survived outside historical migrations" \
  rg -n \
  --type=rust \
  --type=sql \
  --glob '!crates/calm-truth/migrations/**' \
  'FROM runtimes\b|INTO runtimes\b|UPDATE runtimes\b|DELETE FROM runtimes\b' \
  crates/ \
  || failures=$((failures + 1))

scan_must_be_empty \
  "worker_sessions_parity/NEIGE_ASSERT_WORKER_SESSIONS_PARITY_ON_BOOT/worker_sessions_parity_divergences/worker-session-parity-drop symbols survived" \
  rg -n \
  'worker_sessions_parity|NEIGE_ASSERT_WORKER_SESSIONS_PARITY_ON_BOOT|worker_sessions_parity_divergences|worker-session-parity-drop' \
  crates/ \
  || failures=$((failures + 1))

scan_must_be_empty \
  "retired runtime write transaction helper symbols (runtime_start_tx, runtime_supersede_tx, runtime_set_status_for_card_tx, runtime_complete_for_card_tx, runtime_status_flip_tx, runtime_finalize_tx, runtime_lease_acquire_tx, runtime_lease_release_tx, runtime_restore_from_superseded_tx, backfill_worker_sessions_from_runtimes_tx) survived" \
  rg -n \
  --type=rust \
  'runtime_(start|supersede|set_status_for_card|complete_for_card|status_flip|finalize|lease_acquire|lease_release|restore_from_superseded|backfill_worker_sessions_from_runtimes)_tx\b' \
  crates/ \
  || failures=$((failures + 1))

[ "$failures" -eq 0 ] || exit 1
echo "OK: PR9b-iv runtimes retirement ratchet clean"
