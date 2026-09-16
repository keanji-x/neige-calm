#!/usr/bin/env bash
set -euo pipefail
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
output="${1:-$root/target/release/neige-tailnet}"
[[ "$(uname -s)" == Linux ]] || { echo 'neige-tailnet currently supports Linux only' >&2; exit 1; }
mobile_pin="$(awk '$1 == "require" && $2 == "tailscale.com" {print $3}' "$root/mobile/p2p-native/go.mod")"
helper_pin="$(awk '$1 == "require" && $2 == "tailscale.com" {print $3}' "$root/tailnet/go.mod")"
[[ -n "$helper_pin" && "$helper_pin" == "$mobile_pin" ]] || { echo 'Mobile and helper tsnet versions must match' >&2; exit 1; }
mkdir -p "$(dirname -- "$output")"
cd "$root/tailnet"
CGO_ENABLED=0 GOMAXPROCS="${GOMAXPROCS:-4}" go build -mod=readonly -trimpath -tags=ts_omit_logtail -p 4 -o "$output" .
bash "$root/tailnet/verify-build.sh" "$output"
