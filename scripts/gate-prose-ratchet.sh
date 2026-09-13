#!/usr/bin/env bash
# #1635 S6 — the prose ratchet: agent-facing prose must not flow back into Rust.
#
# S1a–S5 moved prompts, tool descriptions and templates out of `*.rs` into
# `prompts/**/*.md` and `templates/**/*.md`. This gate keeps that direction: it
# counts two text shapes in TRACKED `*.rs` under `crates/` and fails when either
# count RISES above the committed baseline, or FALLS below it without the
# baseline being tightened. It is a clone of `gate-1316-terminology-ratchet.sh`
# (`count` verbatim; `--update-baseline`, the baseline format and the compare
# loop copied and compacted; NOT sourced, so the two gates stay independently
# readable and deletable). That script's header lessons apply unchanged:
#   A BASELINE IS ONLY VALID FOR THE TREE IT WAS GENERATED ON. A merge or
#   rebase onto `main` can move a cell either way; regenerate after every
#   upstream sync and treat pre-rebase numbers as expired evidence.
#   actual > baseline => FAIL, new prose entered Rust.
#   actual < baseline => FAIL, run `--update-baseline` and commit the tsv;
#   a baseline left high re-permits every occurrence the change just removed.
#
# WHAT EACH PATTERN COVERS, STATED HONESTLY
# `cjk`  `[\x{4e00}-\x{9fff}]{4,}` — a run of at least four CJK ideographs.
#   Each maximal run is one occurrence: `进程退出后，里保留这么久` is TWO (the
#   comma splits it), `一个 entry` is ZERO. It deliberately does NOT catch
#   English prose, CJK runs shorter than four (identifiers, a two-word
#   comment), kana/hangul (outside the range), or CJK spelled as `\u{...}`.
#   The goal is "no NEW Chinese prompt text in Rust", not "no Chinese": a
#   short CJK comment is not prose an agent reads, and English prompt text is
#   what `long_literal` is for. Text assembled at run time (`concat!`,
#   `format!`) is invisible to any text count; see the 1316 header.
# `long_literal`  `"[^"\\]{120,}` — a `"` followed by at least 120 characters
#   that are neither `"` nor `\`, on ONE line. rustfmt's default width is 100,
#   so such a line is one rustfmt could not wrap: a literal or a comment. This
#   is a text pattern, not a Rust parser, and it over-counts in the REJECT
#   direction, knowingly: the anchoring `"` may be a CLOSING quote (a short
#   literal followed by 120 quote-free, backslash-free characters of code or
#   comment counts — 52 of the 333 baselined occurrences are that shape); SQL
#   and JSON-schema literals count like prose; escaped strings split at every
#   `\`; a `\`-continued multi-line literal is counted per physical line. The
#   exit is the same in every case: break the line, move the text to a
#   `prompts/` or `templates/` `.md`, or argue an enumerated raise below. No
#   pattern here will be widened to exempt a shape.
# SCOPE  `crates/**/*.rs`, tracked files only (`git grep`). `.md` data files
#   under `crates/` are excluded by FILE TYPE, not by directory: they are where
#   the prose is supposed to live. `mod tests` is not separated (#1635 §6.6).
# LOCALE  `\x{4e00}` is above 255, which PCRE2 accepts only in UTF mode, and
#   git takes UTF mode from the locale. Under `LC_ALL=C` the scan is a compile
#   error (exit 128) that `count` refuses — red, not zero — so this script pins
#   `LC_ALL=C.UTF-8` itself instead of trusting the runner.
# RAISES  None taken. A raise, if ever justified, is written here 1316-style:
#   per-file before/after counts, a closed list, one commit naming every
#   constituent line — never a criterion a later commit can re-spend.
# PROVE IT DISCRIMINATES  `--selftest` (assumes the tree is at baseline):
#   a C locale makes `count` fail rather than report 0; one 7-ideograph comment
#   plus one 130-char literal in a probe `.rs` move each cell by EXACTLY +1 and
#   the gate goes red naming both cells; the same text in a probe `.md` under
#   `prompts/` moves neither cell; both probes are gone and the tree is clean
#   afterwards. Every judgement is bash arithmetic or a `case` on captured
#   text — no subprocess sits in an assertion path (a `printf | grep` there
#   produced a 1-in-38 false red in an earlier gate).

set -uo pipefail

cd "$(git rev-parse --show-toplevel)" || exit 1
export LC_ALL=C.UTF-8

SELF='scripts/gate-prose-ratchet.sh'
BASELINE='scripts/gate-prose-ratchet.baseline.tsv'
SCOPE='crates/**/*.rs'
CJK='[\x{4e00}-\x{9fff}]{4,}'
LONG_LITERAL='"[^"\\]{120,}'

# term<TAB>pattern. Reasons for each are in the header above.
read -r -d '' TERMS <<EOF || true
cjk	$CJK
long_literal	$LONG_LITERAL
EOF

# Counts OCCURRENCES, not matching lines (`-o`): appending a second literal to
# an already-matching line must move the count. Verbatim from 1316.
count() { # $1=pattern $2=scope
  local matches grep_status

  matches="$(git grep -P -o -h "$1" -- "$2" ":!$SELF" ":!$BASELINE" 2>/dev/null)"
  grep_status=$?
  case "$grep_status" in
    0) printf '%s\n' "$matches" | wc -l ;;
    1) printf '0\n' ;;
    *)
      echo "::error::git grep failed with exit $grep_status while scanning '$2'; refusing to use a partial count." >&2
      return "$grep_status"
      ;;
  esac
}

ensure_baseline_inputs_are_tracked() {
  local untracked
  untracked="$(git ls-files --others --exclude-standard -- "$SCOPE")" || { echo "::error::could not check $SCOPE for untracked files; refusing to update $BASELINE." >&2; return 1; }
  [ -z "$untracked" ] && return 0
  echo "::error::refusing to update $BASELINE: git grep would omit these untracked files in $SCOPE (git add -N them, then rerun --update-baseline):" >&2
  while IFS= read -r path; do printf '  %s\n' "$path" >&2; done <<<"$untracked"
  return 1
}

emit_baseline() {
  local term pattern found
  echo "# #1635 S6 prose ratchet baseline. Regenerate with:"
  echo "#   ./$SELF --update-baseline"
  echo "# Counts are OCCURRENCES per (term, scope) in tracked *.rs under crates/. Only-down is enforced."
  printf '# term\tscope\tcount\n'
  while IFS=$'\t' read -r term pattern; do
    case "$term" in ''|'#'*) continue ;; esac
    found="$(count "$pattern" "$SCOPE")" || return 1
    printf '%s\t%s\t%s\n' "$term" "$SCOPE" "$found"
  done <<<"$TERMS"
}

if [ "${1:-}" = '--update-baseline' ]; then
  ensure_baseline_inputs_are_tracked || exit 1
  baseline_tmp="$(mktemp "$BASELINE.tmp.XXXXXX")" || exit 1
  trap 'rm -f -- "$baseline_tmp"' EXIT
  trap 'exit 130' HUP INT TERM
  { emit_baseline >"$baseline_tmp" && chmod 0644 "$baseline_tmp" && mv -- "$baseline_tmp" "$BASELINE"; } \
    || { echo "::error::failed to generate $BASELINE; the existing baseline was preserved." >&2; exit 1; }
  trap - EXIT HUP INT TERM
  echo "wrote $BASELINE"
  exit 0
fi

if [ "${1:-}" = '--selftest' ]; then
  probe='crates/calm-types/src/_gate_prose_ratchet_selftest_probe.rs'
  negative='crates/calm-server/prompts/_gate_prose_ratchet_selftest_probe.md'
  # `git grep` reads TRACKED paths only: each probe is `git add -N`ed so it is scanned as a committed file would be.
  cleanup_probes() { git rm -q --cached --force -- "$probe" "$negative" >/dev/null 2>&1; rm -f -- "$probe" "$negative"; }
  trap cleanup_probes EXIT
  fails=0
  ok() { echo "selftest ok: $1"; }
  bad() { echo "SELFTEST FAIL: $1"; fails=1; }

  if LC_ALL=C count "$CJK" "$SCOPE" >/dev/null 2>&1; then bad "under LC_ALL=C the CJK scan reported a count instead of failing — a broken locale would read as clean"
  else ok "a C locale makes the CJK scan fail closed (git grep exit 128), not report 0"; fi

  before_cjk="$(count "$CJK" "$SCOPE")" || exit 1
  before_lit="$(count "$LONG_LITERAL" "$SCOPE")" || exit 1
  { printf '// 提示词散文探针\n'; printf 'const _GATE_PROSE_RATCHET_PROBE: &str = "%0130d";\n' 0; } >"$probe"
  git add -N -- "$probe" || exit 1
  after_cjk="$(count "$CJK" "$SCOPE")" || exit 1
  after_lit="$(count "$LONG_LITERAL" "$SCOPE")" || exit 1
  if [ $((after_cjk - before_cjk)) -eq 1 ]; then ok "one 7-ideograph comment in a probe .rs moves cjk by exactly +1 ($before_cjk -> $after_cjk)"
  else bad "one 7-ideograph comment moved cjk by $((after_cjk - before_cjk)) ($before_cjk -> $after_cjk), expected exactly +1"; fi
  if [ $((after_lit - before_lit)) -eq 1 ]; then ok "one 130-char literal in a probe .rs moves long_literal by exactly +1 ($before_lit -> $after_lit)"
  else bad "one 130-char literal moved long_literal by $((after_lit - before_lit)) ($before_lit -> $after_lit), expected exactly +1"; fi
  gate_output="$("./$SELF" 2>&1)"
  if [ $? -eq 0 ]; then bad "the probe .rs did not trip the gate"
  else case "$gate_output" in
    *"cjk/$SCOPE rose from "*" to $after_cjk occurrences"*"long_literal/$SCOPE rose from "*" to $after_lit occurrences"*)
      ok "the gate is red and names cjk/$SCOPE=$after_cjk and long_literal/$SCOPE=$after_lit" ;;
    *) bad "the gate is red, but not on both cells with the counted numbers:"$'\n'"$gate_output" ;;
  esac; fi

  cleanup_probes
  { printf '提示词散文探针\n'; printf '"%0130d\n' 0; } >"$negative"
  git add -N -- "$negative" || exit 1
  md_cjk="$(count "$CJK" "$SCOPE")" || exit 1
  md_lit="$(count "$LONG_LITERAL" "$SCOPE")" || exit 1
  if [ $((md_cjk - before_cjk)) -eq 0 ] && [ $((md_lit - before_lit)) -eq 0 ]; then ok "the same text in a .md under prompts/ moves neither cell (cjk $md_cjk, long_literal $md_lit)"
  else bad "a .md under prompts/ moved a cell: cjk $before_cjk -> $md_cjk, long_literal $before_lit -> $md_lit"; fi

  cleanup_probes
  trap - EXIT
  leftovers="$(git ls-files --cached --others --exclude-standard -- "$probe" "$negative")"
  if [ -z "$leftovers" ] && [ ! -e "$probe" ] && [ ! -e "$negative" ]; then ok "both probes are gone from the index and the working tree"
  else bad "probe residue after cleanup: ${leftovers:-<files still on disk>}"; fi
  exit "$fails"
fi

[ -f "$BASELINE" ] || { echo "::error::$BASELINE is missing. Generate it with: ./$SELF --update-baseline"; exit 1; }

declare -A EXPECTED=()
while IFS=$'\t' read -r term scope want; do
  case "$term" in ''|'#'*) continue ;; esac
  EXPECTED["$term/$scope"]="$want"
done <"$BASELINE"

fail=0
while IFS=$'\t' read -r term pattern; do
  case "$term" in ''|'#'*) continue ;; esac
  key="$term/$SCOPE"
  got="$(count "$pattern" "$SCOPE")" || { fail=1; continue; }
  want="${EXPECTED[$key]:-}"
  if [ -z "$want" ]; then
    echo "::error::$BASELINE has no row for '$key'. Run --update-baseline."; fail=1
  elif [ "$got" -gt "$want" ]; then
    echo "::error::$key rose from $want to $got occurrences — new '$term' prose entered Rust (#1635 D6: move it to a prompts/ or templates/ .md, or argue an enumerated raise in the header of $SELF)."; fail=1
  elif [ "$got" -lt "$want" ]; then
    echo "::error::$key fell from $want to $got. Tighten the ratchet: ./$SELF --update-baseline, then commit $BASELINE. A baseline left high re-permits every occurrence this change removed."; fail=1
  fi
done <<<"$TERMS"

[ "$fail" -eq 0 ] || { echo "::error::#1635 S6 prose ratchet failed. This is drift control over a stated baseline, not a proof that Rust holds no prose; see the header of $SELF."; exit 1; }
echo "OK: agent-facing prose in *.rs under crates/ is at the #1635 baseline for every term."
