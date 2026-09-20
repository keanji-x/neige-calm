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

# attrs_above <code> <awk-ERE> — print the block of `#[…]` attribute lines *directly above* the first line of <code> matching the pattern. Shared by two gates' adjacency rules.
# Any non-empty non-attribute line resets the block (a decoy `const` between the cfg and the item must break adjacency); a blank line does NOT (callers blank doc/`//` lines, and Rust does not detach an attribute across them); a multi-line attribute stays open until brackets balance (rustfmt wraps `#[cfg(all(…))]`).
# The code blob is fed through a HERE-STRING, never `printf | awk`: the awk `exit`s at the closing line, `printf` takes SIGPIPE, and under `pipefail` 141 becomes this function's status — a false verdict.
attrs_above() {
  awk -v pat="${2?pattern is required}" '
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
  ' <<<"${1?code is required}"
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
