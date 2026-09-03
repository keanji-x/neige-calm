#!/usr/bin/env bash

set -euo pipefail

# #1318 §1 — the shapes that would quietly widen the track-report write
# boundary, checked over the one file that defines it.
#
# ---------------------------------------------------------------------------
# WHAT THIS REPLACES, AND WHY IT IS SO MUCH SMALLER
# ---------------------------------------------------------------------------
#
# It replaces `persist_report_call_sites.sh`, a 423-line per-file census of
# every `persist_report*` mention in the repository. That gate existed because
# the writer was `pub` / `pub(crate)`: any file in the crate could call it, so
# the only way to know the caller set was to enumerate the whole tree. Four
# review rounds found a new way past the enumeration each time — line-broken
# calls, `use ... as` aliases, macro paths, `#[path]` modules, `include!`-
# computed paths, non-`.rs` compiled sources, equal-count substitutions — and
# the root cause was never the regex: a text scan cannot decide what `rustc`
# compiles.
#
# #1318 §1 changed the thing being checked instead, and the honest statement of
# what that bought is two sentences long — one proof, one drift detector.
#
# **The proof.** `track_report::write::persist` is a private `fn`, so code that
# can name it is confined to `mod write` and that module's descendants. Rust
# privacy is per module *subtree*, not per file. Four review rounds looked for a
# counterexample to this half and found none: a proc-macro expanding here
# produces a descendant of `write` (so it is inside the boundary, not around
# it), `include!` content belongs to the module that invoked it, and a
# `#[cfg]`-alternative `write` is a different module in a build where this
# `persist` does not exist. This half carries the slice.
#
# **The drift detector.** Everything below is an attempt to keep that subtree
# equal to one file. It is a text scan, so it is a review aid with the same
# threat model #1300's census declared for itself: it catches somebody adding a
# writer *without knowing this boundary exists*. It does not catch somebody
# working around it, and the KNOWN GAPS section says how.
#
# Do not write "one file" anywhere without "as far as a text scan can tell".
#
# ---------------------------------------------------------------------------
# KNOWN GAPS — declared, not chased
# ---------------------------------------------------------------------------
#
# Four review rounds produced a new bypass each time; the rules grew each time;
# the fifth round would produce another. That is the same loop #1300 exited by
# writing its gaps down, and this file exits it here rather than one round
# later. Every item below is a construction that compiles and stays GREEN:
#
#   G1 **Attribute and derive proc-macros.** `#[derive(InjectDoor)]` or
#      `#[cfg_attr(all(), inject_door)]` expands to whatever it likes, including
#      `pub(crate) mod smuggled { … super::persist(…) … }`. There is no
#      `ident!` for the macro rule to see. `build.rs`-generated input is the
#      same gap wearing a hat.
#
#   G2 **Macro names the regex does not shape-match.** A macro defined
#      elsewhere and invoked as `super::DOOR!()` (all caps) misses the
#      lowercase-anchored pattern, and `super::door` + newline + `!();` is one
#      legal invocation split across two lines.
#
#   G3 **`use … as` split across lines.** The alias rule is line-oriented;
#      `use std::include // …` then `as format;` is legal and invisible.
#
#   G4 **Anything needing a lexer.** The comment stripper does not know string
#      literals (see its own note), and `attrs_above` balances brackets by
#      counting characters, so a `[` inside `#[doc = "…[T"]` unbalances it.
#
# Closing G1–G4 means parsing Rust. The rules here are worth their weight
# against unintentional drift and are not worth another round of regex; if this
# boundary ever needs an adversarial guarantee, the answer is a compile-time
# one (a `trybuild`-style compile-fail suite, or moving the entries behind a
# type only this module can construct), not a longer scan.
#
# ---------------------------------------------------------------------------
# WHAT IT CHECKS, AND WHAT EACH RULE IS FOR
# ---------------------------------------------------------------------------
#
#   R0 The file is readable by rules like these at all: no block comments (a
#      line stripper cannot see through one), no raw identifiers (`r#persist`
#      is `persist` to rustc and not to a regex), no `macro_rules!` (it can
#      expand to a `mod` or to an entry point that appears nowhere literally),
#      no `impl` block (it can carry a `pub(crate)` associated method that
#      reaches the writer while sitting below R3's column-0 anchor). Each of
#      these was a working bypass of an earlier revision of this gate; none is
#      something this file has ever needed.
#
#   R1 `persist` is declared without `pub` (any form) and without a `#[cfg]`.
#      A `pub(crate)` here hands the boundary back to the whole crate and this
#      gate is the only thing that would notice — `rustc` is happy either way.
#      The cfg half (R1b) is what stops a cfg'd-out decoy from being the
#      declaration R1 inspects.
#
#   R2 The file declares no `mod`, no `#[path]`, no `include!` — the second
#      half of the argument above — and no `pub use` (R2b).
#
#      R2b's stated reason used to be wrong and is worth correcting rather than
#      deleting: `pub use self::persist as …` does NOT compile (rustc answers
#      E0364, "`persist` is private, and cannot be re-exported" — verified), so
#      the rule is not holding back a hole rustc leaves open. What it does hold
#      is the export surface: R3 counts `fn` declarations, so a `pub use`
#      carrying some *other* item out of this file is an export R3 never sees.
#
#   R3 The exported entry set is exactly the five pinned `visibility|name`
#      pairs. Adding a sixth door is a legitimate thing to do — it just has to
#      be done in front of a reviewer, which is the same contract the census
#      had, now over five lines instead of the repository.
#
#      #1252 S2 is what that review looks like when it happens:
#      `structural_init_report_tx` was added as the fourth production entry and
#      this list went from four names to five in the same PR. That door does not
#      reach `persist` at all — it shares only the row-write + task-projection
#      pair — so R1's subject is unchanged; what R3 caught, and had to catch, is
#      that the file's exported surface grew.
#
#   R4 `persist_report`, the test-only entry, carries
#      `cfg(any(test, feature = "fixtures"))` **in its own attribute block**,
#      adjacency checked rather than proximity. Without the cfg that entry is a
#      `pub` writer taking a caller-chosen `EditAuthor` in production builds —
#      the exact hole #1300 spent two slices closing.
#
# What it does NOT check, and must not be described as checking:
#
#   * that no *other* code writes the `cards` row directly. A bare
#     `UPDATE cards SET payload = ..., body_crdt = ...` in any
#     `write_with_*_typed` closure would bypass the module and this gate;
#     #1252 §3 P2 records why that has no local solution. Until #1252 S2 the
#     track-create paths were a live instance of this: they wrote the report row
#     through `routes::tracks::persist_initial_report_and_project_tasks_tx`.
#     That function is gone — the create paths now enter this file through
#     `structural_init_report_tx` — so the class stays open while its one known
#     member does not.
#   * **who calls the four entries, or with what.** `agent_report_op` is
#     `pub(crate)` and takes `ActorId` / `EditAuthor` / `auto_promote_draft` /
#     probe from its caller, so a sibling module can compose a combination no
#     production path uses without touching the file this gate reads. Nothing
#     here would notice. That is item 1 of the module's own "What is still not
#     closed"; the carrier for the call sites that exist is
#     `tests/cases/report_write_characterization.rs`, driven through the real
#     router and tool registry.
#   * anything about builds with `--features fixtures`, in which the test-only
#     entry is compiled and public. R4 checks the cfg is present, not that the
#     feature is off.

script_dir="${BASH_SOURCE[0]%/*}"
[ "$script_dir" != "${BASH_SOURCE[0]}" ] || script_dir=.
# shellcheck source=scripts/ci/ratchets/lib.sh
. "$script_dir/lib.sh"

require_tool rg

BOUNDARY_FILE="${REPORT_WRITE_BOUNDARY_FILE:-crates/calm-server/src/track_report/write.rs}"
require_path "$BOUNDARY_FILE"

# The pinned entry set, one `visibility|name` per line. `pub(crate)` entries are
# the production surface; the `pub` one is test-only and R4 additionally
# requires its cfg.
EXPECTED_ENTRIES="pub(crate)|rest_user_replace
pub(crate)|rest_user_block_op
pub(crate)|agent_report_op
pub(crate)|structural_init_report_tx
pub|persist_report"

failures=0
fail() {
  echo "::error::$1"
  failures=$((failures + 1))
}

# Every rule below reads CODE, a copy of the file with whole-line `//` comments
# blanked out (line numbers preserved). Rules that matched the raw file were
# unreadable in a different way: this module's own header names `mod`,
# `#[path]`, `include!` and `pub` in prose, so a rule looking for those had to be
# anchored tightly enough to miss the header — and tight anchoring is exactly
# what the constructions below walked past. Blanking the prose lets the rules be
# blunt.
#
# `/* */` is rejected outright rather than parsed: a block comment can hide a
# declaration from a line-oriented stripper, and this file has never needed one.
# Whole-line `//` only. Trailing-comment stripping was tried in the previous
# revision and is reverted here because it was worse than the problem it solved:
# it has no idea what a string literal is, so
#
#     const _: &str = "https://x"; pub(crate) mod smuggled { … }
#
# got truncated at the `//` inside the URL and R2 never saw the `mod`. A false
# GREEN, introduced by a fix. The cost of reverting is a false RED on a trailing
# `// panic!()` — fail-closed, visible, and fixed by moving the comment to its
# own line. Getting this right needs a lexer, which is the standing argument of
# the KNOWN GAPS section below.
CODE="$(awk '{ if ($0 ~ /^[[:space:]]*\/\//) print ""; else print }' "$BOUNDARY_FILE")"

# attrs_above <awk-ERE> — print the block of `#[…]` attribute lines *directly
# above* the first line matching the pattern, and nothing else.
#
# "Directly above" is the whole point, and it is what a `grep -B N` window got
# wrong. With a window, this passes:
#
#     #[cfg(any(test, feature = "fixtures"))]
#     const CFG_MARKER: () = ();
#     #[allow(clippy::too_many_arguments)]
#     pub async fn persist_report(          // unconditionally public
#
# — the cfg is attached to a const, the function is public in every build, and
# the string is still inside the window. Here any non-attribute line resets the
# block, so the const breaks adjacency and the cfg is not reported.
#
# A blank line does NOT break the block, and that is not laxity: doc comments and
# `//` rationale lines between an attribute and its item are blanked to empty
# lines by the stripper above, so treating a blank as a break made R4 go RED on
# the perfectly ordinary
#
#     #[cfg(any(test, feature = "fixtures"))]
#     /// Test-only direct access to the boundary.
#     pub async fn persist_report(
#
# and made R1b go GREEN when a `// rationale` line sat between the writer's cfg
# and the writer. Rust does not detach an attribute from its item across blank
# lines or comments; neither does this. A non-empty, non-attribute line still
# breaks the block, which is what keeps the decoy-`const` above caught.
#
# A multi-line attribute is one attribute. rustfmt emits them — a long
# `#[cfg(all(\n    feature = "fixtures",\n    unix\n))]` is three lines, only the
# first of which starts with `#[`. Treating the continuation as "some other
# line" cleared the block, which made R1b go GREEN on a cfg'd writer. So the
# block stays open until brackets balance.
attrs_above() {
  printf '%s' "$CODE" | awk -v pat="$1" '
    function depth(s,   i, c, d) {
      d = 0
      for (i = 1; i <= length(s); i++) {
        c = substr(s, i, 1)
        if (c == "[") d++
        else if (c == "]") d--
      }
      return d
    }
    open > 0          { block = block $0 "\n"; open += depth($0); next }
    $0 ~ pat          { print block; exit }
    /^#\[/            { block = block $0 "\n"; open = depth($0); next }
    /^[[:space:]]*$/  { next }
                      { block = "" }
  '
}
if printf '%s' "$CODE" | rg -q '/\*|\*/'; then
  fail "R0: $BOUNDARY_FILE contains a block comment. This gate strips only whole-line \`//\` comments, so a \`/* */\` can hide a declaration from every rule below. Use \`//\`."
fi

# Raw identifiers are rejected for the same reason: `r#persist` *is* `persist` to
# rustc but not to a rule looking for `fn persist(`, so the pair
# (`#[cfg(any())] async fn persist() {}` decoy, `pub(crate) async fn r#persist(`
# real) would leave R1 inspecting the decoy. Nothing in this file needs one.
if printf '%s' "$CODE" | rg -q 'r#'; then
  fail "R0: $BOUNDARY_FILE uses \`r#\` — a raw identifier or a raw string. Both defeat name-based rules: \`r#persist\` *is* \`persist\` to rustc but not to a regex, and a raw string inside an attribute (\`#[doc = r#\"…\"#]\`) can carry text that looks like a second attribute to R4. Neither is needed in this file."
fi

# Aliasing, in either direction. `use std::include as format;` renames a builtin
# macro onto the allowlist — verified to compile on this branch — so a name-based
# allowlist cannot see it. This is the same `use … as` shape that walked past
# #1300's census, which is worth stating plainly: the alias problem did not go
# away with the module boundary, it just moved into this one file.
if printf '%s' "$CODE" | rg -q '\buse\b[^;]*\bas\b'; then
  fail "R0: $BOUNDARY_FILE contains a \`use … as …\` alias. Renaming an item — a macro especially — makes every name-based rule below inspect the wrong name."
fi

# `impl`: an `impl` block can carry a `pub(crate)` associated method that reaches
# `persist` while sitting indented, below R3's column-0 anchor.
if printf '%s' "$CODE" | rg -q '^\s*(impl|(pub(\([^)]*\))?\s+)?trait)\b'; then
  fail "R0: $BOUNDARY_FILE declares an \`impl\` block or a \`trait\`. Both can carry a method that reaches \`persist\` while sitting indented, below R3's column-0 anchor — a \`trait\` with a *default* method is the sharper one, because a sibling implements it with an empty block and then calls the default. If one is genuinely needed, this gate has to grow a rule for it first."
fi

# Macros, by ALLOWLIST rather than by blocklist — and this is the rule that
# actually holds up the `rustc` half of the argument in this gate's header.
#
# A macro *defined elsewhere* and invoked with one line here expands inside this
# module. It was verified on this branch that
#
#     super::door!();      // expands to `pub(crate) mod smuggled { ... }`
#                          // whose body calls super::persist(.., Kernel, ..)
#
# compiles, and that an earlier revision of this gate stayed GREEN on it: the
# blocklist looked for `macro_rules!` *declared here* and for a literal `mod`,
# and this construction has neither. The submodule is real, `persist` is
# reachable from it, and "R2 forbids submodules" was false while it existed.
#
# A blocklist cannot be finished — the macro can be named anything. So the rule
# is inverted: every macro invocation in this file must be on the list below,
# which today is `format!` and nothing else. Adding to the list means arguing
# that the macro cannot expand to an item, in front of a reviewer.
macro_uses="$(
  printf '%s' "$CODE" \
    | rg --line-number --only-matching '(?:[A-Za-z_][A-Za-z0-9_]*::)*[a-z_][a-z0-9_]*!' \
    | rg -v ':format!$' || true
)"
if [ -n "$macro_uses" ]; then
  fail "R0: $BOUNDARY_FILE invokes or defines a macro outside the allowlist (\`format!\`). A macro can expand to a submodule or to an entry point that appears nowhere literally, so neither R2 nor R3 can see it — and a macro defined in another file expands inside this module all the same: $macro_uses"
fi

# --- R1: the writer stays private -------------------------------------------
#
# Anchored at column 0: `persist` is a top-level item, and anchoring keeps the
# rule off the call sites inside the entry bodies (which are indented).
writer_decl="$(printf '%s' "$CODE" | rg --no-line-number '^[[:alnum:]_()[:space:]]*\bfn persist\(' || true)"
if [ -z "$writer_decl" ]; then
  fail "R1: no top-level \`fn persist(\` declaration found in $BOUNDARY_FILE — the boundary this gate defends is not there, so every other rule below is checking nothing"
elif [ "$(printf '%s\n' "$writer_decl" | wc -l)" -ne 1 ]; then
  fail "R1: expected exactly one top-level \`fn persist(\` in $BOUNDARY_FILE, found: $writer_decl"
elif printf '%s' "$writer_decl" | rg -q '\bpub\b'; then
  fail "R1: the writer is declared \`pub\` — that reopens the boundary to the whole crate and rustc will not complain: $writer_decl"
fi

# R1b — the writer must not be `#[cfg]`-conditional. A cfg'd-out `persist` is
# either a decoy (see the raw-identifier note above) or a boundary that exists
# in some builds and not others, and "which builds" is precisely what this gate
# cannot evaluate.
if [ -n "$writer_decl" ] && attrs_above '^[a-zA-Z_ ()]*fn persist[(]' | rg -q '#\[[[:space:]]*cfg'; then
  fail "R1b: the writer carries a \`#[cfg]\` attribute. The boundary must exist in every build, and a cfg'd \`persist\` lets a second, differently-gated one sit beside it."
fi

# --- R2: no declaration that extends this module to another file -------------
escape_hatch="$(printf '%s' "$CODE" | rg --line-number '\bmod\b|#\[\s*path\s*=|(^|[^[:alnum:]_])include!\s*\(' || true)"
if [ -n "$escape_hatch" ]; then
  fail "R2: $BOUNDARY_FILE declares a submodule / \`#[path]\` / \`include!\`, which extends the writer's caller set to source outside this file: $escape_hatch"
fi

# R2b — no re-export. `pub use persist as …;` hands the private writer out under
# a new name without changing its declaration, so R1 stays green.
reexport="$(printf '%s' "$CODE" | rg --line-number '^\s*pub(\([^)]*\))?\s+use\b' || true)"
if [ -n "$reexport" ]; then
  fail "R2b: $BOUNDARY_FILE re-exports something. A \`pub use\` can hand out \`persist\` under another name while its own declaration stays private: $reexport"
fi

# --- R3: the exported entry set is exactly the pinned one --------------------
#
# The visibility group is `pub(?:\([^)]*\))?`, not `pub(?:\(crate\))?`, and the
# `async` is optional. Both were holes in the first revision of this gate: a
# `pub(super) async fn` is visible to `track_report`, which can then `pub use` it
# onward, and a plain `pub(crate) fn` returning a future reaches `persist` just
# as well as an `async fn` does. Neither matched the narrower pattern, so
# neither changed `actual_entries` and R3 stayed green on a new entry. Matching
# every `pub` form means an unexpected one shows up in the diff below rather
# than vanishing from it.
#
# Two shapes walked past the first revision, both verified against this gate:
# `pub(crate) async fn fourth<T>(` (the `<T>` means the name is not followed by
# `(`, so a pattern demanding one saw nothing) and
# `pub(crate) async unsafe fn fifth(` (a qualifier the alternation did not list).
# Hence: every qualifier is optional and repeatable, and the capture stops at the
# name rather than requiring what comes after it.
#
# Reads CODE, not the raw file — a `pub(crate) async fn …(` quoted in this
# module's own prose would otherwise be counted as an entry.
actual_entries="$(
  printf '%s' "$CODE" | rg --no-line-number --replace '$1|$2' \
    '^(pub(?:\([^)]*\))?)\s+(?:(?:async|unsafe|const|extern\s+"[^"]*")\s+)*fn\s+([A-Za-z_][A-Za-z0-9_]*).*$' || true
)"
if [ "$actual_entries" != "$EXPECTED_ENTRIES" ]; then
  fail "R3: the exported write-entry set changed. A new write shape is a real decision and has to be reviewed as one — update EXPECTED_ENTRIES in this gate in the same PR.
expected:
$EXPECTED_ENTRIES
actual:
$actual_entries"
fi

# --- R4: the test-only entry keeps its cfg ----------------------------------
#
# The cfg must be in the attribute block *attached to* the function, not merely
# somewhere nearby — see `attrs_above` for the decoy that "nearby" admits.
if ! printf '%s' "$CODE" | rg -q '^pub async fn persist_report\('; then
  fail "R4: no \`pub async fn persist_report(\` found — if the test entry was renamed or removed, update R4 and EXPECTED_ENTRIES together"
elif ! attrs_above '^pub async fn persist_report[(]' | rg -q '^#\[cfg\(any\(test, feature = "fixtures"\)\)\]$'; then
  fail "R4: the test-only \`persist_report\` entry does not carry \`#[cfg(any(test, feature = \"fixtures\"))]\` in its own attribute block — without it, production builds get a \`pub\` writer that takes a caller-chosen EditAuthor"
fi

if [ "$failures" -ne 0 ]; then
  echo "::error::report-write boundary gate: $failures rule(s) failed"
  exit 1
fi

echo "OK: the track-report write boundary in $BOUNDARY_FILE holds its four pinned shapes (private writer, no module escape hatch, five exported entries, test entry cfg-gated)"
