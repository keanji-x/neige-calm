#!/usr/bin/env bash
# The server and maintained frontend must declare the same compatibility version.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

RUST=crates/calm-server/src/routes/version.rs
FE=fe/web/src/app/providers/public.tsx

extract() { # <file> <regex with one capture group>
  local file="$1" re="$2" value
  value="$(grep -oP "$re" "$file" | head -n1 || true)"
  if [ -z "$value" ]; then
    echo "::error::web compat lockstep: could not read WEB_COMPAT_VERSION from $file — this gate is scanning nothing" >&2
    exit 1
  fi
  printf '%s' "$value"
}

rust_v="$(extract "$RUST" '(?<=pub const WEB_COMPAT_VERSION: u32 = )[0-9]+')"
fe_v="$(extract "$FE" '(?<=export const WEB_COMPAT_VERSION = )[0-9]+')"

if [ "$rust_v" != "$fe_v" ]; then
  echo "::error::WEB_COMPAT_VERSION drift: $RUST=$rust_v, $FE=$fe_v — both must be equal"
  exit 1
fi

echo "OK: WEB_COMPAT_VERSION == $rust_v in both declarations ($RUST, $FE)"
