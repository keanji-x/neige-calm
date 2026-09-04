#!/usr/bin/env bash

set -euo pipefail

require_tool() {
  local tool="${1:?tool name is required}"

  command -v "$tool" >/dev/null 2>&1 || {
    echo "::error::required tool '$tool' not found in PATH"
    exit 1
  }
}

require_path() {
  local path

  for path in "$@"; do
    [ -e "$path" ] || {
      echo "::error::required scan path does not exist: $path"
      exit 1
    }
  done
}

# attrs_above <code> <awk-ERE> — print the block of `#[…]` attribute lines
# *directly above* the first line of <code> matching the pattern, and nothing
# else. Lives here because two gates need the same adjacency semantics
# (`report_write_boundary.sh` R1b/R4, `append_seam_boundary.sh` D1) and a second
# copy would be a re-derivation of a rule that has already been defeated twice.
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
# lines by the callers' comment stripper, so treating a blank as a break made
# `report_write_boundary.sh` R4 go RED on the perfectly ordinary
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
  printf '%s' "${1?code is required}" | awk -v pat="${2?pattern is required}" '
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

scan_must_be_empty() {
  local rule_label="${1:?rule label is required}"
  local output
  local rc
  shift

  set +e
  output="$("$@" 2>&1)"
  rc=$?
  set -e

  case "$rc" in
    0)
      printf '%s\n' "$output"
      echo "::error::$rule_label"
      return 1
      ;;
    1)
      return 0
      ;;
    *)
      printf '%s\n' "$output"
      echo "::error::$rule_label; scan infrastructure failed with exit $rc"
      return 1
      ;;
  esac
}
