#!/usr/bin/env bash
# Regression for entry.sh's required-CLI preflight (`assert_required_tools_on_path`): it must FAIL CLOSED (exit 73) when any bare CLI the contained agent stack shells out to is missing from PATH.
# Pure shell, no docker: source entry.sh, point PATH at a dir of executable stubs built FROM `REQUIRED_PATH_TOOLS`, and assert the exit contract.
set -uo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source-path=SCRIPTDIR
# shellcheck source=entry.sh
source "$SCRIPT_DIR/entry.sh"

# Sourcing entry.sh turns on `set -e`; the assertions drive functions that return non-zero.
set +e

FAILS=0
ok()  { printf '[check_tools] ok   : %s\n' "$1"; }
bad() { printf '[check_tools] FAIL : %s\n' "$1" >&2; FAILS=$((FAILS + 1)); }

# A controlled PATH holding one executable stub per required tool. `command -v`
# only resolves executables on PATH, so this dir alone satisfies the gate.
TOOLBOX="$(mktemp -d)"
trap 'rm -rf -- "$TOOLBOX"' EXIT
make_stub() { printf '#!/bin/sh\n:\n' > "$TOOLBOX/$1"; chmod +x "$TOOLBOX/$1"; }
for t in "${REQUIRED_PATH_TOOLS[@]}"; do make_stub "$t"; done

# Drive the gate with a chosen PATH in a subshell so its exit code is
# observable (fail -> exit 73) instead of killing this harness. `command -v`
# is a bash builtin, so the empty-PATH case still executes the gate correctly.
run_gate() { # $1 = PATH value -> echoes exit code
    ( PATH="$1"; assert_required_tools_on_path ) >/dev/null 2>&1
    printf '%s' "$?"
}
assert_exit() { # $1 label $2 want $3 got
    if [ "$2" = "$3" ]; then ok "$1 (exit $3)"; else bad "$1: want exit $2, got $3"; fi
}

# All required tools present -> pass.
assert_exit "all required tools present -> pass" 0 "$(run_gate "$TOOLBOX")"

# Each tool missing in turn -> fail closed (73). This also proves the gate
# actually reads the live list, not a hardcoded name.
for t in "${REQUIRED_PATH_TOOLS[@]}"; do
    rm -f -- "$TOOLBOX/$t"
    assert_exit "missing '$t' -> fail closed" 73 "$(run_gate "$TOOLBOX")"
    make_stub "$t"
done

# Empty PATH -> everything missing -> fail closed.
assert_exit "empty PATH -> fail closed" 73 "$(run_gate "")"

if [ "$FAILS" -eq 0 ]; then
    printf '[check_tools] ALL PASS\n'
    exit 0
fi
printf '[check_tools] %d FAILURE(S)\n' "$FAILS" >&2
exit 1
