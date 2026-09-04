#!/usr/bin/env bash
# #1316 S4b — `SYNC_EVENT_VERSION` and the migrations' `event_version` stamps
# must agree.
#
# WHY THIS EXISTS
#
# Two independent declarations of one number:
#
#   crates/calm-types/src/event.rs          pub const SYNC_EVENT_VERSION
#   crates/calm-truth/migrations/*.sql      UPDATE events SET ... event_version = N
#
# A migration that rewrites an event KIND stamps the rewritten rows with the
# version a client must be at to understand the new discriminator (0038, 0080,
# 0081, and 0094 all do this). The client then drops every frame whose
# `eventVersion` exceeds the `syncEventVersion` the server advertises — which
# is `SYNC_EVENT_VERSION` verbatim (`routes/version.rs`).
#
# So if the migration stamps 16 while the constant stays 15, every row the
# migration touched is PERMANENTLY invisible: the frames are read out of the
# database, shipped, and discarded by the client's own gate. Nothing goes red.
# The Rust tests only ever see the constant; the migration tests only ever see
# the literal; no test in the tree compares them. The failure is a silently
# truncated conversation history on a live database, discovered by a human.
#
# WHAT THIS GATE ENFORCES — exactly three rules, no more.
#
# All three read only EXECUTABLE SQL: `--` comments are stripped before any
# literal is extracted, so a `event_version = N` written in prose satisfies
# nothing. (Without that, a migration whose only literal sits in a comment
# stamps zero rows while looking compliant.)
#
#   R1 NO LITERAL ABOVE THE CONSTANT. No `event_version = N` anywhere under
#      the migrations dir may exceed `SYNC_EVENT_VERSION`. A stamp above the
#      constant is the data-loss direction: the client gate discards those
#      rows outright.
#
#   R2 THE NEWEST STAMPING MIGRATION IS PINNED, LITERAL BY LITERAL. Take the
#      highest-numbered migration file that stamps at all; EVERY literal in it
#      must equal `SYNC_EVENT_VERSION`. Not the maximum — every one. A file
#      whose statements disagree with each other (one at 15, its siblings at
#      16) has a maximum that still matches the constant, so a max-equality
#      check passes it while the rows stamped by the odd statement out are
#      visible to a client that cannot classify them. Equality also catches
#      the reverse drift: a constant bumped without the accompanying
#      migration.
#
#   R3 A KIND REWRITE MUST RAISE THE VERSION. Every `UPDATE events` statement
#      that assigns `kind` must assign `event_version` in the same statement,
#      and the version a migration stamps on its kind rewrites must be
#      STRICTLY GREATER than every literal stamped by any earlier-numbered
#      migration. Equality is not enough: a new migration that renames a kind
#      while restamping the version already in force ships a new discriminator
#      to a client that is exactly at that version — the client accepts the
#      frame (its gate is `eventVersion > syncEventVersion`), fails to
#      classify the new tag, and advances its cursor past a row it never
#      rendered. R2 then forces the newest such stamp to be the constant, so
#      R1+R2+R3 together mean: rename a kind => raise the version => raise the
#      constant.
#
# NOT ENFORCED (deliberate scope): whether a kind rewrite exists at all for a
# given Rust-side rename, whether payload-key rewrites carry a stamp (0094 §3
# stamps them; 0083 §3/§4 deliberately does not), and anything about tables
# other than `events`. `UPDATE operations SET kind` (0083 §5) and
# `UPDATE cards SET kind` (0081) are outside R3 by construction — they are not
# the client's frame discriminator.
#
# HOW IT PARSES. Textually, not with a SQL parser: `--` to end of line is
# dropped, then `;` splits statements. Two assumptions, both true of every file
# under the migrations dir today and both cheap to re-check: no migration puts
# `--` or `;` inside a quoted string literal. If one ever does, the split
# misreads that file — which surfaces as a spurious failure here, not as a
# silent pass, because every rule below is a "must equal"/"must exist" test.
#
# `--selftest` runs six single-edit mutations against throwaway copies and
# asserts this script rejects every one. Each mutation is ONE edit: one
# literal, or one new file. A mutation that rewrites every literal at once
# cannot distinguish R2 from a max-equality check.

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

RUST=crates/calm-types/src/event.rs
MIGRATIONS=crates/calm-truth/migrations

# Executable SQL of one file: `--` comments removed, blank lines dropped.
exec_sql() { # <file>
  sed -E 's/--.*$//' "$1" | sed -E '/^[[:space:]]*$/d'
}

# One SQL statement per output line, comments already stripped.
exec_statements() { # <file>
  exec_sql "$1" | tr '\n' ' ' | tr ';' '\n' | sed -E '/^[[:space:]]*$/d'
}

# Every `event_version = N` literal in executable SQL, one per line.
file_literals() { # <file>
  exec_sql "$1" | grep -oP '(?<![\w])event_version\s*=\s*\K[0-9]+' || true
}

# The SET clause of an `UPDATE events` statement: everything between `SET` and
# the first `WHERE` (or end of statement). Non-greedy, so a `WHERE` cannot be
# swallowed by a later one.
set_clause() { # reads statement on stdin
  grep -oiP '^\s*UPDATE\s+events\s+SET\s+\K.*?(?=\s+WHERE\s|$)' || true
}

check() { # <rust-file> <migrations-dir>
  local rust="$1" migrations="$2" const_v
  local -a files=()

  const_v="$(grep -oP '(?<=pub const SYNC_EVENT_VERSION: u32 = )[0-9]+' "$rust" | head -n1 || true)"
  if [ -z "$const_v" ]; then
    echo "::error::sync event version lockstep: could not read SYNC_EVENT_VERSION from $rust — this gate is scanning nothing" >&2
    return 1
  fi

  while IFS= read -r f; do
    if [ -n "$f" ]; then files+=("$f"); fi
  done < <(find "$migrations" -maxdepth 1 -name '*.sql' | sort)

  if [ "${#files[@]}" -eq 0 ]; then
    echo "::error::sync event version lockstep: no .sql files under $migrations — this gate is scanning nothing" >&2
    return 1
  fi

  # --- Collect, per file, the executable literals and the kind-rewrite stamps.
  # `seen_max` is the highest literal in any file processed SO FAR. Inside the
  # per-file R3 loop it is therefore "everything strictly earlier"; after the
  # loop it is the global maximum.
  local newest_stamping="" seen_max=""
  local -a stamping_files=()
  local f lits stmt setc s_lits l

  for f in "${files[@]}"; do
    lits="$(file_literals "$f")"

    # R3: every `UPDATE events ... SET ... kind = ...` must stamp, and the
    # stamp must clear every literal stamped by an earlier-numbered file.
    while IFS= read -r stmt; do
      [ -n "$stmt" ] || continue
      grep -qiP '^\s*UPDATE\s+events\s' <<<"$stmt" || continue
      setc="$(set_clause <<<"$stmt")"
      grep -qP '(?<![\w])kind\s*=' <<<"$setc" || continue
      s_lits="$(grep -oP '(?<![\w])event_version\s*=\s*\K[0-9]+' <<<"$setc" || true)"
      if [ -z "$s_lits" ]; then
        echo "::error::$f: an 'UPDATE events SET kind = ...' statement does not stamp event_version — a client at the current syncEventVersion accepts the frame, cannot classify the new kind, and advances its cursor past a row it never rendered. Statement: $(cut -c1-160 <<<"$stmt")"
        return 1
      fi
      while IFS= read -r l; do
        [ -n "$l" ] || continue
        if [ -n "$seen_max" ] && [ "$l" -le "$seen_max" ]; then
          echo "::error::$f: a kind rewrite stamps event_version = $l, but an earlier-numbered migration already stamped $seen_max — a kind rewrite MUST raise the version strictly, or a client sitting at $seen_max accepts the frame and silently drops it as unclassifiable"
          return 1
        fi
      done <<<"$s_lits"
    done < <(exec_statements "$f")

    if [ -n "$lits" ]; then
      stamping_files+=("$f")
      newest_stamping="$f"
      while IFS= read -r l; do
        [ -n "$l" ] || continue
        if [ -z "$seen_max" ] || [ "$l" -gt "$seen_max" ]; then seen_max="$l"; fi
      done <<<"$lits"
    fi
  done

  if [ "${#stamping_files[@]}" -eq 0 ]; then
    echo "::error::sync event version lockstep: no executable 'event_version = N' literal found under $migrations — this gate is scanning nothing" >&2
    return 1
  fi

  # --- R1: nothing anywhere may exceed the constant.
  if [ "$seen_max" -gt "$const_v" ]; then
    echo "::error::SYNC_EVENT_VERSION drift: $rust=$const_v but a migration under $migrations stamps event_version = $seen_max — every row stamped above the constant is dropped by the client's eventVersion gate"
    return 1
  fi

  # --- R2: every literal in the newest stamping migration equals the constant.
  local bad=""
  while IFS= read -r l; do
    [ -n "$l" ] || continue
    [ "$l" = "$const_v" ] || bad+="$l "
  done <<<"$(file_literals "$newest_stamping")"
  if [ -n "$bad" ]; then
    echo "::error::SYNC_EVENT_VERSION drift: $rust=$const_v but the newest stamping migration $newest_stamping contains event_version literal(s) ${bad% } — EVERY literal in it must equal the constant, not merely its maximum; a statement stamping a different value either loses its rows to the client gate or ships an unclassifiable frame"
    return 1
  fi

  echo "OK: SYNC_EVENT_VERSION == $const_v; every executable event_version literal in $newest_stamping equals it, nothing under $migrations exceeds it, and every 'UPDATE events SET kind' stamp strictly raises the version"
}

# --- selftest helpers -------------------------------------------------------

# Print the newest .sql under <dir> that carries an executable literal.
newest_stamping_file() { # <migrations-dir>
  local f out=""
  while IFS= read -r f; do
    if [ -n "$(file_literals "$f")" ]; then out="$f"; fi
  done < <(find "$1" -maxdepth 1 -name '*.sql' | sort)
  printf '%s' "$out"
}

# Rewrite exactly ONE `event_version = <from>` literal (the first) to <to>.
mutate_one_literal() { # <file> <from> <to>
  local file="$1" from="$2" to="$3" tmpf
  tmpf="$(mktemp)"
  awk -v from="$from" -v to="$to" '
    BEGIN { done = 0 }
    {
      if (!done && sub("event_version = " from, "event_version = " to)) done = 1
      print
    }
    END { if (!done) exit 3 }
  ' "$file" > "$tmpf"
  mv "$tmpf" "$file"
}

# Rewrite exactly ONE `event_version = <from>` literal — the LAST one in the
# file — to <to>. Used where the mutation must land on a statement that is not
# a kind rewrite, so that R2 (per-literal equality) is the rule under test.
mutate_last_literal() { # <file> <from> <to>
  local file="$1" from="$2" to="$3" tmpf line
  line="$(grep -nP "(?<![\w])event_version = ${from}(?![0-9])" "$file" \
          | grep -vP '^[0-9]+:.*--.*event_version' | tail -n1 | cut -d: -f1)"
  if [ -z "$line" ]; then return 3; fi
  tmpf="$(mktemp)"
  awk -v ln="$line" -v from="$from" -v to="$to" '
    NR == ln { sub("event_version = " from, "event_version = " to) }
    { print }
  ' "$file" > "$tmpf"
  mv "$tmpf" "$file"
}

expect_reject() { # <label> <rust> <migrations> <why>
  local label="$1" rust="$2" migrations="$3" why="$4" status=0
  check "$rust" "$migrations" >/dev/null 2>&1 || status=$?
  if [ "$status" -eq 0 ]; then
    echo "::error::selftest: mutation '$label' was ACCEPTED — $why" >&2
    return 1
  fi
  echo "  rejected: $label"
}

selftest() {
  local tmp const_v bumped lowered newest base
  tmp="$(mktemp -d)"
  # shellcheck disable=SC2064
  trap "rm -rf '$tmp'" RETURN

  # Baseline: the real tree must pass, otherwise the mutations below prove
  # nothing.
  if ! check "$RUST" "$MIGRATIONS" >/dev/null; then
    echo "::error::selftest: the unmutated tree already fails — fix that first" >&2
    return 1
  fi

  const_v="$(grep -oP '(?<=pub const SYNC_EVENT_VERSION: u32 = )[0-9]+' "$RUST" | head -n1)"
  bumped="$(( const_v + 1 ))"
  lowered="$(( const_v - 1 ))"
  cp "$RUST" "$tmp/event.rs"

  fresh_migrations() { # <name> -> prints the dir
    local d="$tmp/$1"
    rm -rf "$d"
    mkdir -p "$d"
    cp "$MIGRATIONS"/*.sql "$d/"
    printf '%s' "$d"
  }

  # 1. Bump the constant alone. R2: the newest stamping migration still says
  #    $const_v.
  sed -E "s/(pub const SYNC_EVENT_VERSION: u32 = )[0-9]+/\1${bumped}/" "$RUST" > "$tmp/event_bumped.rs"
  base="$(fresh_migrations m1)"
  expect_reject "constant bumped alone ($const_v -> $bumped)" \
    "$tmp/event_bumped.rs" "$base" \
    "the constant may not move without the migration that stamps it"

  # 2. Bump ONE migration literal alone, upward. R1 (and R2).
  base="$(fresh_migrations m2)"
  newest="$(newest_stamping_file "$base")"
  mutate_one_literal "$newest" "$const_v" "$bumped"
  expect_reject "one migration literal bumped alone ($const_v -> $bumped in $(basename "$newest"))" \
    "$tmp/event.rs" "$base" \
    "a stamp above SYNC_EVENT_VERSION makes those rows invisible to every client"

  # 3. An empty migrations dir must be reported as "scanning nothing".
  mkdir -p "$tmp/m3"
  expect_reject "empty migrations dir" "$tmp/event.rs" "$tmp/m3" \
    "this gate can then scan nothing and pass"

  # 4. (A1) COMMENT-ONLY STAMP. One new migration that renames a kind and
  #    writes its version in a `--` comment instead of in the SQL. A gate that
  #    greps the raw file sees a literal equal to the constant and passes,
  #    while ZERO rows are stamped.
  base="$(fresh_migrations m4)"
  cat > "$base/0095_selftest_comment_only_stamp.sql" <<EOF
-- selftest fixture: the stamp lives only in prose.
UPDATE events SET kind = 'selftest.renamed' WHERE kind = 'selftest.legacy';  -- event_version = ${const_v}
EOF
  expect_reject "kind rewrite whose only event_version literal is inside a -- comment" \
    "$tmp/event.rs" "$base" \
    "comments are not executable SQL; that migration stamps nothing"

  # 5. (A2) KIND REWRITE THAT DOES NOT RAISE THE VERSION. One new migration
  #    renaming a kind while restamping the version already in force. Max
  #    equality passes it; a client at exactly that version accepts the frame,
  #    cannot classify the new tag, and loses the row.
  base="$(fresh_migrations m5)"
  cat > "$base/0095_selftest_kind_rewrite_no_bump.sql" <<EOF
UPDATE events SET kind = 'selftest.renamed', event_version = ${const_v} WHERE kind = 'selftest.legacy';
EOF
  expect_reject "kind rewrite restamping the version already in force ($const_v)" \
    "$tmp/event.rs" "$base" \
    "a rename must raise the version strictly or the new discriminator reaches a client that cannot read it"

  # 6. (A3) LOCAL DECOUPLING. Lower exactly ONE literal in the newest stamping
  #    migration; its siblings keep the constant, so the file's MAXIMUM is
  #    unchanged. The LAST literal is chosen so the mutation lands on a
  #    payload-rewrite statement rather than a kind rewrite: R2 (per-literal
  #    equality), not R3, is the rule under test here.
  base="$(fresh_migrations m6)"
  newest="$(newest_stamping_file "$base")"
  mutate_last_literal "$newest" "$const_v" "$lowered"
  expect_reject "one literal lowered in $(basename "$newest") ($const_v -> $lowered), maximum unchanged" \
    "$tmp/event.rs" "$base" \
    "max-equality hides a single decoupled statement; the rows it stamps are not at the version the constant promises"

  echo "OK: selftest — all six single-edit mutations were rejected"
}

if [ "${1:-}" = "--selftest" ]; then
  selftest
else
  check "$RUST" "$MIGRATIONS"
fi
