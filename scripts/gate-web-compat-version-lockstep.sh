#!/usr/bin/env bash
# #1209 PR-2 acceptance B6 — the three `WEB_COMPAT_VERSION` constants must agree.
#
# WHY THIS EXISTS
#
# `WEB_COMPAT_VERSION` is declared in three places:
#
#   crates/calm-server/src/routes/version.rs   (served as webCompatVersion AND
#                                               minWebCompatVersion)
#   web/src/api/version.ts                     (the bundle in production today)
#   fe/web/src/app/providers/public.tsx        (the bundle not yet in production)
#
# Nothing relates them. Before this gate existed, all three drift directions
# were CI-green, because each side only tests its own local constant:
#
#   server 16, both bundles 17  -> the cached old bundle is NOT blocked and
#                                  keeps sending requests that cannot succeed;
#                                  the whole hard-fail ruling is inert
#   server 17, either bundle 16 -> that bundle shows "please refresh" forever,
#                                  and refreshing re-downloads the same 16
#   one bundle 17, server 16    -> the new bundle passes and the old one is let
#                                  through too, i.e. "partially works" again
#
# Deriving the two frontend constants from one source would be better than
# testing them for equality, but that means wiring codegen into two frontends;
# #1209 §3.6 chose this and recorded the tradeoff.
#
# Positive/negative pair: all three equal => pass; change ANY ONE of them =>
# fail. (On the pre-#1209 tree, every one of those single-edit mutations was
# green — which is the reason this file exists.)

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

RUST=crates/calm-server/src/routes/version.rs
WEB=web/src/api/version.ts
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
web_v="$(extract "$WEB" '(?<=export const WEB_COMPAT_VERSION = )[0-9]+')"
fe_v="$(extract "$FE" '(?<=export const WEB_COMPAT_VERSION = )[0-9]+')"

if [ "$rust_v" != "$web_v" ] || [ "$rust_v" != "$fe_v" ]; then
  echo "::error::WEB_COMPAT_VERSION drift: $RUST=$rust_v, $WEB=$web_v, $FE=$fe_v — all three must be equal"
  exit 1
fi

echo "OK: WEB_COMPAT_VERSION == $rust_v in all three declarations ($RUST, $WEB, $FE)"
