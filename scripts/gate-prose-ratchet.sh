#!/usr/bin/env bash
# Prose ratchet: agent-facing prose must not flow back into Rust. Counts two text
# shapes (`cjk`: a run of ≥4 ideographs; `long_literal`: a `"` followed by 120+ quote-free, backslash-free chars)
# in `*.rs` under `crates/` (tracked and untracked, not ignored) and fails when a cell rises above the committed
# baseline or falls below it without `--update-baseline` (a baseline is only valid for the tree it was generated on).
# `LC_ALL=C.UTF-8` is load-bearing: `\x{4e00}` needs PCRE2 UTF mode (else git grep exits 128) and `long_literal` counts BYTES outside it.
# The `:(glob)` pathspec magic is load-bearing: without it `crates/**/*.rs` misses a `crates/foo.rs`.

# long-term-goal adds eight CJK matches in independent test expectations, not
# agent-facing Rust prose: two title assertions (templates/tests.rs and the
# track_template_tracks.rs roster), two KR heading assertions (templates/tests.rs),
# and two matches each in the auto-merge assertions (builtin_plugins/tests.rs
# and track_template_tracks.rs). Keeping these literals independent pins the
# supplied template contract; importing expectations from it would mask drift.
# Only changes to that contract require updating these assertions and this
# existing baseline. No new scan or CI invocation is added.

# Objective budget localization adds two CJK matches: the native table title
# "阶段预算" in track_planner_context/objective_budget.rs and its independent
# expectation in tests/cases/mcp_track_report_live_slots.rs. These are visible UI
# copy and a contract assertion, not agent-facing instructions. Only a title
# contract change edits these two sites; this keeps the existing single scan.

# Objective measurement adds three CJK matches: "最新判定", "进行中评估" and
# "账本条数" in track_planner_context/objective_kr.rs. These are native table
# headings, not agent instructions. Only a heading-contract change edits that
# projection and this existing baseline; adding an objective edits neither.
# No new scan, list of objective keys or CI invocation is added.

# KR current/target adds six CJK matches in native table display copy and
# independent production-entry assertions. These are UI labels, not agent
# instructions. A display-contract change edits the projection, its expectations
# and this existing baseline; adding an objective adds no scan or CI invocation.

# KR evaluator rules add eight CJK matches: three in native display copy
# (majority ties, per-severity lower median, and the nonempty blocking-free pass
# condition), and five in independent majority/median target expectations.
# These describe engine rules without evaluating them. A display-contract change
# edits the projection, its expectations and this existing baseline; adding a
# rubric item adds no scan, exemption or CI invocation.

# Evaluator binding adds seven long_literal matches: three persistence queries
# and four crash-fixture trigger statements. Agent instructions live in prompts/.
# They use the existing 120-character shape and baseline; there is no new
# exemption, identity list or CI invocation to maintain.

set -uo pipefail

cd "$(git rev-parse --show-toplevel)" || exit 1
export LC_ALL=C.UTF-8

SELF='scripts/gate-prose-ratchet.sh'
BASELINE='scripts/gate-prose-ratchet.baseline.tsv'
SCOPE='crates/**/*.rs'
PATHSPEC=":(glob)$SCOPE"
CJK='[\x{4e00}-\x{9fff}]{4,}'
LONG_LITERAL='"[^"\\]{120,}'

# `--baseline <path>` compares against another tsv (the selftest's malformed
# copies). The exclusion pathspecs stay on the canonical paths regardless.
BASELINE_FILE="$BASELINE"
if [ "${1:-}" = '--baseline' ]; then
  BASELINE_FILE="${2:?--baseline needs a path}"
  shift 2
fi

# term<TAB>pattern.
read -r -d '' TERMS <<EOF || true
cjk	$CJK
long_literal	$LONG_LITERAL
EOF

# Shared mechanics; vocabulary, scope and error context remain in this entrypoint.
source scripts/text-ratchet-engine.sh
count() { ratchet_count "$@" detail; }
RATCHETED_SCOPES=("$SCOPE")
RATCHETED_PATHS=("$PATHSPEC")

emit_baseline() {
  echo "# #1635 S6 prose ratchet baseline. Regenerate with:"
  echo "#   ./$SELF --update-baseline"
  echo "# Counts are OCCURRENCES per (term, scope) in non-ignored *.rs under crates/. Only-down is enforced."
  printf '# term\tscope\tcount\n'
  ratchet_emit_rows
}

if [ "${1:-}" = '--update-baseline' ]; then
  ratchet_update_baseline "" "failed to generate $BASELINE; the existing baseline was preserved."
  exit "$?"
fi

if [ "${1:-}" = '--selftest' ]; then
  PROBES=(crates/calm-types/src/_gate_prose_ratchet_selftest_probe.rs
          crates/_gate_prose_selftest_probe.rs
          crates/calm-server/prompts/_gate_prose_ratchet_selftest_probe.md)
  # Never overwrite or delete anything this invocation did not create.
  for p in "${PROBES[@]}"; do
    if [ -e "$p" ] || [ -L "$p" ] || [ -n "$(git ls-files -- "$p")" ]; then
      echo "::error::selftest probe path '$p' already exists (tracked, untracked, or a symlink); refusing to run rather than touch it."
      exit 1
    fi
  done
  # Probes stay untracked: the scan must count a new file that was never `git add`ed.
  created=()
  make_probe() { # $1=path $2=content — O_EXCL via noclobber: an existing file or (dangling) symlink is refused, never written through
    if [ -L "$1" ] || ! ( set -o noclobber; printf '%s' "$2" >"$1" ); then echo "::error::refusing to write selftest probe '$1': something is already at that path"; return 1; fi
    created+=("$1")
  }
  printf -v rs_probe_text '// 提示词散文探针\nconst _GATE_PROSE_RATCHET_PROBE: &str = "%0130d";\n' 0
  printf -v md_probe_text '提示词散文探针\n"%0130d\n' 0
  cleanup_probes() {
    local p residue
    for p in "${created[@]}"; do rm -f -- "$p"; done
    created=()
    residue="$(git status --porcelain -- "${PROBES[@]}")"
    [ -z "$residue" ] || { echo "::error::selftest probe residue after cleanup:"$'\n'"$residue"; return 1; }
  }
  tsvdir="$(mktemp -d)" || exit 1
  trap 'rm -rf -- "$tsvdir"; cleanup_probes || exit 1' EXIT
  fails=0
  ok() { echo "selftest ok: $1"; }
  bad() { echo "SELFTEST FAIL: $1"; fails=1; }

  # A dangling symlink at a probe path is invisible to `-e` and to `git ls-files`; this one is our own, so removing it is allowed.
  ln -s "$tsvdir/dangling-target" "${PROBES[1]}" || exit 1
  symlink_output="$("./$SELF" --selftest 2>&1)" && { bad "a nested --selftest ran with a dangling symlink at ${PROBES[1]}"; }
  case "$symlink_output" in
    *"::error::selftest probe path '${PROBES[1]}' already exists"*) ok "a dangling symlink at ${PROBES[1]} makes --selftest refuse, naming it" ;;
    *) bad "a dangling symlink at ${PROBES[1]} was not refused by name:"$'\n'"$symlink_output" ;;
  esac
  if [ -L "${PROBES[1]}" ] && [ ! -e "$tsvdir/dangling-target" ]; then ok "the symlink is still there and nothing was written through it"
  else bad "the refused --selftest touched the symlink or its target"; fi
  rm -f -- "${PROBES[1]}"

  if LC_ALL=C count "$CJK" "$PATHSPEC" >/dev/null 2>&1; then bad "under LC_ALL=C the CJK scan reported a count instead of failing — a broken locale would read as clean"
  else ok "a C locale makes the CJK scan fail closed (git grep exit 128), not report 0"; fi

  # An exact copy is the positive control; each variant differs from the committed tsv in exactly one way.
  bad_tsv() { # $1=variant  (copy of $BASELINE on stdout, one defect applied)
    local line last=''
    while IFS= read -r line; do
      case "$1" in
        nonint) case "$line" in cjk*) line="${line}x" ;; esac ;;
        huge)   case "$line" in cjk*) line="${line%$'\t'*}"$'\t'9223372036854775808 ;; esac ;;
        crlf)   line="${line}"$'\r' ;;
        extra)  case "$line" in cjk*) line="${line}"$'\t'9 ;; esac ;;
      esac
      printf '%s\n' "$line"; last="$line"
    done <"$BASELINE"
    [ "$1" = dup ] && printf '%s\n' "$last"
  }
  # Each negative must be red BY ITS OWN VALIDATOR, so deleting one guard turns
  # exactly one case red instead of being covered by a sibling guard.
  for variant in copy nonint huge crlf dup extra; do
    bad_tsv "$variant" >"$tsvdir/$variant.tsv"
    tsv_output="$("./$SELF" --baseline "$tsvdir/$variant.tsv" 2>&1)"
    tsv_status=$?
    case "$variant" in
      nonint) want_msg='is not a non-negative integer' ;;
      huge)   want_msg='has more than 12 digits' ;;
      crlf)   want_msg='contains a carriage return' ;;
      dup)    want_msg='duplicates the row for' ;;
      extra)  want_msg='tab(s), expected exactly 2' ;;
      *)      want_msg='' ;;
    esac
    if [ "$variant" = copy ]; then
      if [ "$tsv_status" -eq 0 ]; then ok "an exact copy of the tsv via --baseline is green (positive control)"
      else bad "an exact copy of the tsv via --baseline was red:"$'\n'"$tsv_output"; fi
    elif [ "$tsv_status" -eq 0 ]; then bad "a malformed tsv ($variant) was accepted as green"
    else case "$tsv_output" in
      *"::error::$tsvdir/$variant.tsv:"*"$want_msg"*) ok "a malformed tsv ($variant) is red, by its own validator ('$want_msg'), naming the row" ;;
      *) bad "a malformed tsv ($variant) is red, but not by '$want_msg' naming the row:"$'\n'"$tsv_output" ;;
    esac; fi
  done

  before_cjk="$(count "$CJK" "$PATHSPEC")" || exit 1
  before_lit="$(count "$LONG_LITERAL" "$PATHSPEC")" || exit 1
  for probe in "${PROBES[0]}" "${PROBES[1]}"; do
    make_probe "$probe" "$rs_probe_text" || exit 1
    after_cjk="$(count "$CJK" "$PATHSPEC")" || exit 1
    after_lit="$(count "$LONG_LITERAL" "$PATHSPEC")" || exit 1
    if [ $((after_cjk - before_cjk)) -eq 1 ]; then ok "one 7-ideograph comment in $probe moves cjk by exactly +1 ($before_cjk -> $after_cjk)"
    else bad "one 7-ideograph comment in $probe moved cjk by $((after_cjk - before_cjk)) ($before_cjk -> $after_cjk), expected exactly +1"; fi
    if [ $((after_lit - before_lit)) -eq 1 ]; then ok "one 130-char literal in $probe moves long_literal by exactly +1 ($before_lit -> $after_lit)"
    else bad "one 130-char literal in $probe moved long_literal by $((after_lit - before_lit)) ($before_lit -> $after_lit), expected exactly +1"; fi
    gate_output="$("./$SELF" 2>&1)"
    if [ $? -eq 0 ]; then bad "the probe $probe did not trip the gate"
    else case "$gate_output" in
      *"cjk/$SCOPE rose from "*" to $after_cjk occurrences"*"long_literal/$SCOPE rose from "*" to $after_lit occurrences"*)
        ok "the gate is red on $probe and names cjk/$SCOPE=$after_cjk and long_literal/$SCOPE=$after_lit" ;;
      *) bad "the gate is red on $probe, but not on both cells with the counted numbers:"$'\n'"$gate_output" ;;
    esac; fi
    cleanup_probes || fails=1
  done

  make_probe "${PROBES[2]}" "$md_probe_text" || exit 1
  md_cjk="$(count "$CJK" "$PATHSPEC")" || exit 1
  md_lit="$(count "$LONG_LITERAL" "$PATHSPEC")" || exit 1
  if [ $((md_cjk - before_cjk)) -eq 0 ] && [ $((md_lit - before_lit)) -eq 0 ]; then ok "the same text in a .md under prompts/ moves neither cell (cjk $md_cjk, long_literal $md_lit)"
  else bad "a .md under prompts/ moved a cell: cjk $before_cjk -> $md_cjk, long_literal $before_lit -> $md_lit"; fi

  if cleanup_probes; then ok "every probe is gone: git status --porcelain on the probe paths is empty"
  else fails=1; fi
  trap 'rm -rf -- "$tsvdir"' EXIT
  exit "$fails"
fi

load_baseline "$BASELINE_FILE" || exit 1

ratchet_rose_error() {
  echo "::error::$key rose from $want to $got occurrences — new '$term' prose entered Rust (#1635 D6: move it to a prompts/ or templates/ .md, or argue an enumerated raise in the header of $SELF)."
}
ratchet_compare

[ "$fail" -eq 0 ] || { echo "::error::#1635 S6 prose ratchet failed. This is drift control over a stated baseline, not a proof that Rust holds no prose; see the header of $SELF."; exit 1; }
echo "OK: agent-facing prose in *.rs under crates/ is at the #1635 baseline for every term."
