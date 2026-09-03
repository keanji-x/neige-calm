#!/usr/bin/env bash
# Classify a NUL-delimited git path stream conservatively for CI fan-out.
#
# Only documentation is allowed to return false. Everything else, an empty diff, or a classifier
# failure must fan out to the full code CI. `docs/oracle/**` is intentionally documentation here:
# the mutation plan still runs on every PR and validates the oracle catalog before it can emit an
# empty plan.
#
# Silent boundary: executable/generated inputs placed below docs/ would be treated as docs. The
# repository currently has no such input; adding one requires narrowing this allowlist first.
set -euo pipefail

mode="${1:-code}"
case "$mode" in
  code|rust) ;;
  *) echo "unknown change-classification mode: $mode" >&2; exit 2 ;;
esac

seen=false
relevant=false
while IFS= read -r -d '' path; do
  seen=true
  if [ "$mode" = code ]; then
    case "$path" in
      docs/*|README.md|*/README.md|CHANGELOG.md|CONTRIBUTING.md|SECURITY.md|CODE_OF_CONDUCT.md|LICENSE|LICENSE.*|NOTICE|NOTICE.*) ;;
      *) relevant=true ;;
    esac
  else
    case "$path" in
      *.rs|Cargo.toml|Cargo.lock|*/Cargo.toml|rust-toolchain|rust-toolchain.toml|rustfmt.toml|.cargo/*|.config/nextest.toml|crates/*|plugins/*|scripts/*|e2e/*|docker/*|Makefile|docker-compose.yml|.github/workflows/*) relevant=true ;;
      *) ;;
    esac
  fi
done

if [ "$seen" = false ]; then
  relevant=true
fi

printf '%s\n' "$relevant"
