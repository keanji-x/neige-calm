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

seen=false
code_changed=false
while IFS= read -r -d '' path; do
  seen=true
  case "$path" in
    docs/*|README.md|*/README.md|CHANGELOG.md|CONTRIBUTING.md|SECURITY.md|CODE_OF_CONDUCT.md|LICENSE|LICENSE.*|NOTICE|NOTICE.*) ;;
    *) code_changed=true ;;
  esac
done

if [ "$seen" = false ]; then
  code_changed=true
fi

printf '%s\n' "$code_changed"
