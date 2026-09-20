#!/usr/bin/env bash
# `SYNC_EVENT_VERSION` (crates/calm-types/src/event.rs) and the migrations' executable
# `event_version = N` stamps must agree: a stamp above the constant makes every row it
# touched permanently invisible to the client's eventVersion gate, and nothing else goes red.
# R1: no literal exceeds the constant. R2: EVERY literal in the newest stamping migration
# equals it (not merely the maximum). R3: an `UPDATE events` assigning `kind` must stamp a
# version strictly above every earlier migration's. Parsing is textual (`--` stripped, `;` splits).

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

RUST=crates/calm-types/src/event.rs
MIGRATIONS=crates/calm-truth/migrations

exec_sql() { # <file>
  sed -E 's/--.*$//' "$1" | sed -E '/^[[:space:]]*$/d'
}

exec_statements() { # <file>
  exec_sql "$1" | tr '\n' ' ' | tr ';' '\n' | sed -E '/^[[:space:]]*$/d'
}

file_literals() { # <file>
  exec_sql "$1" | grep -oP '(?<![\w])event_version\s*=\s*\K[0-9]+' || true
}

# Non-greedy so a `WHERE` cannot be swallowed by a later one.
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

  # `seen_max` is the highest literal in files processed SO FAR: inside the R3 loop it is "everything strictly earlier", after the loop the global maximum.
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

newest_stamping_file() { # <migrations-dir>
  local f out=""
  while IFS= read -r f; do
    if [ -n "$(file_literals "$f")" ]; then out="$f"; fi
  done < <(find "$1" -maxdepth 1 -name '*.sql' | sort)
  printf '%s' "$out"
}

# One past the highest-numbered migration, zero-padded: a planted fixture must sort after every real migration, or R3 judges it against an older `seen_max` and the mutation stops modelling its hazard.
next_migration_prefix() { # <migrations-dir>
  local f n max=0
  while IFS= read -r f; do
    n="$(basename "$f" | grep -oP '^[0-9]+' || true)"
    [ -n "$n" ] || continue
    n=$((10#$n))
    if [ "$n" -gt "$max" ]; then max="$n"; fi
  done < <(find "$1" -maxdepth 1 -name '*.sql')
  if [ "$max" -eq 0 ]; then
    echo "::error::selftest: no numbered .sql under $1 — a planted fixture cannot be ordered against anything" >&2
    return 1
  fi
  printf '%04d' "$((max + 1))"
}

# Fail unless <file> is the last .sql in sort order under <dir> — the premise every planted-fixture mutation rests on.
assert_sorts_last() { # <migrations-dir> <fixture-path>
  local dir="$1" fixture="$2" last
  last="$(find "$dir" -maxdepth 1 -name '*.sql' | sort | tail -n1)"
  if [ "$last" != "$fixture" ]; then
    echo "::error::selftest: planted fixture $(basename "$fixture") does not sort last under $dir (last is $(basename "$last")) — R3 would judge it against an older maximum, so this mutation no longer models the hazard it names" >&2
    return 1
  fi
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

# Rewrite exactly ONE literal — the LAST one — so the mutation lands on a statement that is not a kind rewrite and R2 is the rule under test.
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
  local tmp const_v bumped lowered newest base fixture
  tmp="$(mktemp -d)"
  # shellcheck disable=SC2064
  trap "rm -rf '$tmp'" RETURN

  # Baseline: the real tree must pass, otherwise the mutations below prove nothing.
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

  # 1. Bump the constant alone (R2).
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

  # 4. Comment-only stamp: a gate that greps the raw file sees a literal equal to the constant while ZERO rows are stamped.
  base="$(fresh_migrations m4)"
  fixture="$base/$(next_migration_prefix "$base")_selftest_comment_only_stamp.sql"
  cat > "$fixture" <<EOF
-- selftest fixture: the stamp lives only in prose.
UPDATE events SET kind = 'selftest.renamed' WHERE kind = 'selftest.legacy';  -- event_version = ${const_v}
EOF
  assert_sorts_last "$base" "$fixture"
  expect_reject "kind rewrite whose only event_version literal is inside a -- comment" \
    "$tmp/event.rs" "$base" \
    "comments are not executable SQL; that migration stamps nothing"

  # 5. Kind rewrite restamping the version already in force: max-equality passes it.
  base="$(fresh_migrations m5)"
  fixture="$base/$(next_migration_prefix "$base")_selftest_kind_rewrite_no_bump.sql"
  cat > "$fixture" <<EOF
UPDATE events SET kind = 'selftest.renamed', event_version = ${const_v} WHERE kind = 'selftest.legacy';
EOF
  assert_sorts_last "$base" "$fixture"
  expect_reject "kind rewrite restamping the version already in force ($const_v)" \
    "$tmp/event.rs" "$base" \
    "a rename must raise the version strictly or the new discriminator reaches a client that cannot read it"

  # 6. Lower exactly ONE literal in the newest stamping migration so the file's MAXIMUM is unchanged; R2, not R3, is under test.
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
