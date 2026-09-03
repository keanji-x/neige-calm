#!/usr/bin/env bash

set -euo pipefail

# #1318 §1 — fixtures for `report_write_boundary.sh`.
#
# A ratchet nobody has watched fail is a ratchet nobody knows works. Two
# disciplines here, both inherited from the census selftest this replaces:
#
#   * **One violation per red case.** Every red fixture is the green fixture
#     with exactly one line changed, and names the rule id it must trip. A
#     fixture that breaks two rules at once cannot tell you which one is live —
#     that is how the old census shipped a case that stayed green after its
#     rule was deleted.
#   * **Red for the stated reason.** Each red case pins a substring of the
#     message. Exit-1-for-any-reason is not evidence: the gate exits 1 when the
#     file is missing too.
#
# The green fixture is not a hand-written miniature — it is the real
# `crates/calm-server/src/wave_report/write.rs`. A miniature would drift from
# production and this suite would keep passing while the gate stopped matching
# the file it actually runs on.

script_dir="${BASH_SOURCE[0]%/*}"
[ "$script_dir" != "${BASH_SOURCE[0]}" ] || script_dir=.
script_dir="$(cd "$script_dir" && pwd)"
repo_root="$(cd "$script_dir/../../.." && pwd)"

gate="$script_dir/report_write_boundary.sh"
[ -x "$gate" ] || { echo "::error::gate not executable: $gate"; exit 1; }

real_boundary="$repo_root/crates/calm-server/src/wave_report/write.rs"
[ -f "$real_boundary" ] || {
  echo "::error::production boundary file missing: $real_boundary"
  exit 1
}

failures=0
cases=0

# run_case <name> <green|red> <expected-substring-when-red> <sed-program>
#
# Applies the sed program to a copy of the real boundary file, points the gate
# at the copy, and checks the exit class (plus the message, for red cases).
run_case() {
  local name="$1" expect="$2" want_msg="$3" program="$4"
  cases=$((cases + 1))
  local dir; dir="$(mktemp -d)"
  local file="$dir/write.rs"
  sed "$program" "$real_boundary" > "$file"

  if [ "$expect" = red ] && cmp -s "$file" "$real_boundary"; then
    echo "FAIL [$name]: the mutation changed nothing — the sed program no longer matches the production file, so this case is testing the green fixture"
    failures=$((failures + 1))
    rm -rf "$dir"
    return
  fi

  local output rc
  set +e
  output="$(cd "$repo_root" && REPORT_WRITE_BOUNDARY_FILE="$file" "$gate" 2>&1)"
  rc=$?
  set -e

  case "$expect" in
    green)
      if [ "$rc" -eq 0 ]; then
        echo "PASS [$name]: green"
      else
        echo "FAIL [$name]: expected green, got exit $rc"
        printf '%s\n' "$output"
        failures=$((failures + 1))
      fi
      ;;
    red)
      if [ "$rc" -eq 0 ]; then
        echo "FAIL [$name]: expected red, gate passed"
        failures=$((failures + 1))
      elif ! printf '%s' "$output" | grep -qF "$want_msg"; then
        echo "FAIL [$name]: red, but not for the stated reason (wanted substring: $want_msg)"
        printf '%s\n' "$output"
        failures=$((failures + 1))
      else
        echo "PASS [$name]: red on \"$want_msg\""
      fi
      ;;
  esac
  rm -rf "$dir"
}

# The production file, unmodified, must pass. Without this the red cases could
# all be passing because the gate is broken in general.
run_case "green: production boundary as-is" green "" ""

# R1 — the writer goes `pub(crate)`. This is the shape the old census could
# never see coming and the one that silently restores the pre-#1318 world.
run_case "R1: writer becomes pub(crate)" red \
  "R1: the writer is declared \`pub\`" \
  's/^async fn persist($/pub(crate) async fn persist(/'

# R1 — the writer is renamed away, so nothing is being defended.
run_case "R1: writer renamed" red \
  "R1: no top-level \`fn persist(\` declaration found" \
  's/^async fn persist($/async fn persist_inner(/'

# R2 — a submodule declaration. The submodule's file is not read by this gate,
# and code in it can call the private writer.
run_case "R2: submodule declared" red \
  "R2:" \
  '/^use super::\*;$/a\
mod helper;'

# R2 — the `#[path]` form of the same escape.
run_case "R2: #[path] module" red \
  "R2:" \
  '/^use super::\*;$/a\
#[path = "../elsewhere.rs"] mod helper;'

# R2 — the `include!` form.
run_case "R2: include! of another file" red \
  "R2:" \
  '/^use super::\*;$/a\
include!("../elsewhere.rs");'

# R3 — a fourth production entry appears. Legitimate, and must be seen.
run_case "R3: a new pub(crate) entry" red \
  "R3: the exported write-entry set changed" \
  '/^use super::\*;$/a\
pub(crate) async fn kernel_restamp(repo: \&dyn RouteRepo) -> Result<Card, CalmError> { unimplemented!() }'

# R3 — an existing entry is removed. The ratchet has to bite in both
# directions: a one-way ratchet is a ratchet with no pawl.
run_case "R3: an entry is removed" red \
  "R3: the exported write-entry set changed" \
  's/^pub(crate) async fn agent_report_op($/async fn agent_report_op(/'

# R3 — a new entry in a visibility form the gate's first revision did not match.
# `pub(super)` is visible to `wave_report`, which can `pub use` it onward; the
# original `pub(?:\(crate\))?` group let this one through silently.
run_case "R3: a new pub(super) entry" red \
  "R3: the exported write-entry set changed" \
  '/^use super::\*;$/a\
pub(super) async fn sneaky(repo: \&dyn RouteRepo) -> Result<Card, CalmError> { unimplemented!() }'

# R3 — a new non-async entry. A `pub(crate) fn` returning a future reaches
# `persist` exactly as well as an `async fn` does; the original pattern required
# the `async` keyword and so never saw it.
run_case "R3: a new non-async entry" red \
  "R3: the exported write-entry set changed" \
  '/^use super::\*;$/a\
pub(crate) fn sneaky_sync(repo: \&dyn RouteRepo) -> Result<Card, CalmError> { unimplemented!() }'

# R4 — the test-only entry loses its cfg and becomes a production `pub` writer
# that takes a caller-chosen EditAuthor. Note this trips R4 only: R3 pins the
# `pub|persist_report` line, which is unchanged.
run_case "R4: test entry loses its cfg" red \
  "R4: the test-only \`persist_report\` entry does not carry" \
  's/^#\[cfg(any(test, feature = "fixtures"))\]$//'

# R4 — the construction that defeated the first revision's `grep -B 4` window:
# the cfg stays in the file, four lines above, but attaches to a decoy const.
# The function is public in every build. Only adjacency catches this.
run_case "R4: cfg detached onto a decoy const" red \
  "R4: the test-only \`persist_report\` entry does not carry" \
  '/^#\[cfg(any(test, feature = "fixtures"))\]$/a\
const CFG_MARKER: () = ();'

# R0 — a block comment. The rules strip whole-line `//` comments only, so a
# `/* */` can hide a declaration from every one of them.
run_case "R0: block comment" red \
  "R0:" \
  '/^use super::\*;$/a\
/* nothing to see here */'

# R0 — a raw identifier. `r#persist` is `persist` to rustc but not to a rule
# looking for `fn persist(`, which is what lets a cfg-d-out decoy be the
# declaration R1 inspects while the real writer is public.
run_case "R0: raw identifier" red \
  "R0:" \
  '/^use super::\*;$/a\
const r#type: u8 = 0;'

# R0 — `macro_rules!`. `items! { mod helper; }` is a legitimate submodule
# declaration that appears nowhere literally, so R2 cannot see it.
run_case "R0: macro_rules!" red \
  "R0:" \
  '/^use super::\*;$/a\
macro_rules! items { ($($i:item)*) => {$($i)*}; }'

# R0 — an `impl` block. Its associated methods are indented, so a
# `pub(crate) async fn` inside one is an entry point R3 never sees.
run_case "R0: impl block" red \
  "R0:" \
  '/^use super::\*;$/a\
impl Wave {}'

# R1b — the writer becomes `#[cfg]`-conditional. R1 still finds exactly one
# non-pub `fn persist(`, so only the adjacency check notices.
run_case "R1b: writer gains a cfg" red \
  "R1b:" \
  '/^async fn persist($/i\
#[cfg(feature = "fixtures")]'

# R2b — a re-export hands the private writer out under another name. Its own
# declaration is untouched, so R1 stays green.
run_case "R2b: pub use re-export" red \
  "R2b:" \
  '/^use super::\*;$/a\
pub use self::persist as escape_hatch;'

echo "----"
echo "$cases case(s), $failures failure(s)"
[ "$failures" -eq 0 ] || exit 1
