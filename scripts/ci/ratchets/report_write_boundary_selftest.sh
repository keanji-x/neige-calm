#!/usr/bin/env bash

set -euo pipefail

# Fixtures for `report_write_boundary.sh`. Every red case is the real production file with one line changed and pins a substring of its rule's message (exit 1 alone is not evidence). Some cases trip two rules; each still pins the rule it is for, and every failure is printed. The substring test is bash's own `case`, not `printf | grep -qF`, which flaked roughly once in forty runs.

script_dir="${BASH_SOURCE[0]%/*}"
[ "$script_dir" != "${BASH_SOURCE[0]}" ] || script_dir=.
script_dir="$(cd "$script_dir" && pwd)"
repo_root="$(cd "$script_dir/../../.." && pwd)"

gate="$script_dir/report_write_boundary.sh"
[ -x "$gate" ] || { echo "::error::gate not executable: $gate"; exit 1; }

real_boundary="$repo_root/crates/calm-server/src/track_report/write.rs"
[ -f "$real_boundary" ] || {
  echo "::error::production boundary file missing: $real_boundary"
  exit 1
}

failures=0
cases=0

# run_case <name> <green|red> <expected-substring-when-red> <sed-program>: applies the sed program to a copy of the real boundary file and points the gate at it.
run_case() {
  local name="$1" expect="$2" want_msg="$3" program="$4"
  cases=$((cases + 1))
  local dir; dir="$(mktemp -d)"
  local file="$dir/write.rs"
  sed "$program" "$real_boundary" > "$file"

  # Applies to GREEN cases too: a green case whose sed silently stops matching degrades into the first case and tests nothing.
  if [ -n "$program" ] && cmp -s "$file" "$real_boundary"; then
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
      elif case "$output" in *"$want_msg"*) false ;; *) true ;; esac; then
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

# The production file, unmodified, must pass — or every red case could be red because the gate is broken in general.
run_case "green: production boundary as-is" green "" ""

# The explicit User start entry is admitted by exact name, never by pattern.
run_case "red: User start entry replaced by another writer" red "R3:" \
  's/pub(crate) async fn rest_user_start(/pub(crate) async fn unreviewed_start(/'

# R1 — the writer goes `pub(crate)`.
run_case "R1: writer becomes pub(crate)" red \
  "R1: the writer is declared \`pub\`" \
  's/^async fn persist($/pub(crate) async fn persist(/'

# R1 — the writer is renamed away, so nothing is being defended.
run_case "R1: writer renamed" red \
  "R1: no top-level \`fn persist(\` declaration found" \
  's/^async fn persist($/async fn persist_inner(/'

# R2 — a submodule declaration; its file is not read by this gate, and code in it can call the private writer.
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

# The approved bounded repair door remains an exact named entry, not a wildcard.
run_case "R3: Planner repair door removed" red \
  "R3: the exported write-entry set changed" \
  's/^pub(crate) async fn planner_repair(/async fn planner_repair(/'
run_case "R3: Planner repair door replaced by arbitrary writer" red \
  "R3: the exported write-entry set changed" \
  's/^pub(crate) async fn planner_repair(/pub(crate) async fn arbitrary_writer(/'

run_case "R3: a new pub(crate) entry" red \
  "R3: the exported write-entry set changed" \
  '/^use super::\*;$/a\
pub(crate) async fn kernel_restamp(repo: \&dyn RouteRepo) -> Result<Card, CalmError> { unimplemented!() }'

# R3 — an existing entry is removed; the ratchet has to bite in both directions.
run_case "R3: an entry is removed" red \
  "R3: the exported write-entry set changed" \
  's/^pub(crate) async fn agent_report_op($/async fn agent_report_op(/'

# R3 — the structural door is removed: the line that was added last, so it has been watched fail too.
run_case "R3: the structural door is removed" red \
  "R3: the exported write-entry set changed" \
  's/^pub(crate) async fn structural_init_report_tx($/async fn structural_init_report_tx(/'

# R3 — `pub(super)` is visible to `track_report`, which can `pub use` it onward.
run_case "R3: a new pub(super) entry" red \
  "R3: the exported write-entry set changed" \
  '/^use super::\*;$/a\
pub(super) async fn sneaky(repo: \&dyn RouteRepo) -> Result<Card, CalmError> { unimplemented!() }'

# R3 — a new non-async entry: proves R3 sees the shape (a real one would return `impl Future`).
run_case "R3: a new non-async entry" red \
  "R3: the exported write-entry set changed" \
  '/^use super::\*;$/a\
pub(crate) fn sneaky_sync(repo: \&dyn RouteRepo) -> Result<Card, CalmError> { unimplemented!() }'

# R4 — the test-only entry loses its cfg; trips R4 only, since R3's `pub|persist_report` line is unchanged.
run_case "R4: test entry loses its cfg" red \
  "R4: the test-only \`persist_report\` entry does not carry" \
  's/^#\[cfg(any(test, feature = "fixtures"))\]$//'

# R4 — the cfg stays in the file, four lines above, but attaches to a decoy const; only adjacency catches this.
run_case "R4: cfg detached onto a decoy const" red \
  "R4: the test-only \`persist_report\` entry does not carry" \
  '/^#\[cfg(any(test, feature = "fixtures"))\]$/a\
const CFG_MARKER: () = ();'

# R0 — a block comment can hide a declaration from a whole-line `//` stripper.
run_case "R0: block comment" red \
  "R0:" \
  '/^use super::\*;$/a\
/* nothing to see here */'

# R0 — a raw identifier: `r#persist` is `persist` to rustc but not to a rule looking for `fn persist(`.
run_case "R0: raw identifier" red \
  "R0:" \
  '/^use super::\*;$/a\
const r#type: u8 = 0;'

# R0 — `macro_rules!` declared here (the declaration half; the invocation half is further down). The rule rejects the construct rather than expanding it.
run_case "R0: macro_rules!" red \
  "R0:" \
  '/^use super::\*;$/a\
macro_rules! items { ($($i:item)*) => {$($i)*}; }'

# R0 — an `impl` block, empty on purpose to keep the case to a single violation: associated methods are indented, below R3's column-0 anchor.
run_case "R0: impl block" red \
  "R0:" \
  '/^use super::\*;$/a\
impl Track {}'

# R1b — R1 still finds exactly one non-pub `fn persist(`, so only the adjacency check notices.
run_case "R1b: writer gains a cfg" red \
  "R1b:" \
  '/^async fn persist($/i\
#[cfg(feature = "fixtures")]'

# R2b — `pub use self::persist as …` does NOT compile (E0364), so this proves the rule fires, not a bypass; what R2b defends is the export surface R3 never sees.
run_case "R2b: pub use re-export" red \
  "R2b:" \
  '/^use super::\*;$/a\
pub use self::persist as escape_hatch;'

# R0 — a macro invocation defined elsewhere. The fixture does not build the macro, so this proves the rule fires on this spelling, not that the class is covered.
run_case "R0: macro invocation defined elsewhere" red \
  "R0:" \
  '/^use super::\*;$/a\
super::door!();'

# R3 — a generic entry: `<T>` means the name is not followed by `(`.
run_case "R3: generic entry hides the paren" red \
  "R3: the exported write-entry set changed" \
  '/^use super::\*;$/a\
pub(crate) async fn fourth<T>(repo: \&dyn RouteRepo) -> Result<Card, CalmError> { unimplemented!() }'

# R3 — a qualifier an alternation might not list.
run_case "R3: unsafe entry" red \
  "R3: the exported write-entry set changed" \
  '/^use super::\*;$/a\
pub(crate) async unsafe fn fifth(repo: \&dyn RouteRepo) -> Result<Card, CalmError> { unimplemented!() }'

# R0 — a raw string: a multi-line `#[doc = r#"…"#]` whose body reads as an attribute lets an unanchored search find the cfg *inside the string* while the function is public in every build.
run_case "R0: raw string in an attribute" red \
  "R0:" \
  '/^use super::\*;$/a\
#[doc = r#"inert"#]\
const DOC_DECOY: () = ();'

# R0 — `use std::include as format;` renames a builtin macro onto the allowlist and compiles.
run_case "R0: use-as alias" red \
  "R0:" \
  '/^use super::\*;$/a\
use std::include as format;'

# GREEN — a doc comment between an attribute and its item is ordinary Rust; the stripper blanks it and `attrs_above` must not treat the blank as a reset.
run_case "green: doc comment between the cfg and the fn" green "" \
  's|^#\[cfg(any(test, feature = "fixtures"))\]$|#[cfg(any(test, feature = "fixtures"))]\
/// rationale line that used to break adjacency|'

# R1b — a rustfmt-wrapped multi-line `#[cfg(all(…))]` on the writer; bracket-aware adjacency is what catches it.
run_case "R1b: writer gains a multi-line cfg" red \
  "R1b:" \
  '/^async fn persist($/i\
#[cfg(all(\
    feature = "fixtures",\
    unix\
))]'

echo "----"
echo "$cases case(s), $failures failure(s)"
[ "$failures" -eq 0 ] || exit 1
