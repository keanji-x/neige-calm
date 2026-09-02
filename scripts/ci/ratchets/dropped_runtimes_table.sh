#!/usr/bin/env bash

set -euo pipefail

# Complements the PR9b-iv ratchet (which forbids only the bare,
# unquoted FROM/INTO/UPDATE/DELETE runtimes verbs). The `runtimes`
# table was dropped in migration 0055 and must never return. This
# script closes the two holes the verb-list ratchet leaves: (a) DDL/JOIN
# it never names (CREATE TABLE / CREATE INDEX ... ON / ALTER TABLE /
# REPLACE INTO / JOIN), and (b) QUOTED identifiers `"runtimes"` that
# slip past every \b-anchored pattern. The 0055 drop-regression test
# legitimately re-creates the table to prove the drop, so it is the
# single scoped exclusion (alongside historical migrations).

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
  "$scan_root/crates/calm-truth/migrations" \
  "$scan_root/crates/calm-server/tests/cases/migration_0055_drop_runtimes.rs"

cd "$scan_root"

exclusions=(
  --type=rust
  --type=sql
  --glob '!crates/calm-truth/migrations/**'
  --glob '!crates/calm-server/tests/cases/migration_0055_drop_runtimes.rs'
)
failures=0

# (a) DDL / JOIN / REPLACE — bare or quoted — that the verb-list ratchet misses.
scan_must_be_empty \
  "dropped 'runtimes' table reintroduced via DDL/JOIN/REPLACE (migration 0055 dropped it)" \
  rg -niP \
  "${exclusions[@]}" \
  '\b((create\s+(temp(orary)?\s+)?table|alter\s+table|replace\s+into|join)\s+(if\s+not\s+exists\s+)?|create\s+(unique\s+)?index\s+(if\s+not\s+exists\s+)?\S+\s+on\s+)"?runtimes"?\b' \
  crates/ \
  || failures=$((failures + 1))

# (b) Quoted-identifier DML that bypasses the existing \b-anchored
# FROM/INTO/UPDATE/DELETE gate.
scan_must_be_empty \
  "dropped 'runtimes' table reintroduced via quoted-identifier SQL" \
  rg -niP \
  "${exclusions[@]}" \
  '\b(from|update|delete\s+from|insert\s+into)\s+"runtimes"' \
  crates/ \
  || failures=$((failures + 1))

[ "$failures" -eq 0 ] || exit 1
echo "OK: no dropped-runtimes-table reintroduction (quoted/DDL/JOIN holes closed)"
