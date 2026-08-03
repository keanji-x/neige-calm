#!/bin/sh
set -eu

implementation=tools/mock/generator.ts
baseline=$(mktemp)
cp "$implementation" "$baseline"
trap 'cp "$baseline" "$implementation"; rm -f "$baseline"' EXIT

mutate() {
  name=$1
  before=$2
  after=$3
  test_name=$4
  log=$(mktemp)
  cmp -s "$implementation" "$baseline" || { echo "$name: implementation differs from guarded baseline" >&2; exit 2; }
  BEFORE="$before" AFTER="$after" node -e 'const fs=require("fs");const p=process.argv[1];const s=fs.readFileSync(p,"utf8");if(!s.includes(process.env.BEFORE))process.exit(3);fs.writeFileSync(p,s.replace(process.env.BEFORE,process.env.AFTER))' "$implementation"
  if npx vitest run tools/mock/generator.test.ts --reporter=verbose >"$log" 2>&1; then
    echo "$name: SURVIVED" >&2
    exit 1
  fi
  cp "$baseline" "$implementation"
  grep -F "$test_name" "$log" >/dev/null || { echo "$name: expected killer missing: $test_name" >&2; exit 4; }
  echo "$name: killed; failing cases:"
  grep '^ ×\|^ FAIL ' "$log" || true
  rm -f "$log"
}

mutate leading-slash "path.startsWith('/')" "path.includes('/')" 'attributes every violation in no-leading-slash'
mutate unmatched-close "if (character === '}')" "if (character === '#')" 'attributes every violation in unmatched-close'
mutate missing-path-parameter "for (const name of parsed.parameters) if (!declared.includes(name))" "for (const name of []) if (!declared.includes(name))" 'reports both directions of path-parameter mismatch'
mutate required-responses "if (!object(operation.responses) || Object.keys(operation.responses).length === 0)" "if (false)" 'attributes every violation in no-responses'
mutate manual-dispatch-exemption "segments[1] === 'generated' || segments[1] === 'scenarios'" "segments[1] === 'generated' || segments[1] === 'adapter.ts'" 'defines the PR2 checkpoint'
