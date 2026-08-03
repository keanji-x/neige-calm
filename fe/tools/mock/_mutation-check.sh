#!/bin/sh
set -eu

temporary_root=$(mktemp -d "${TMPDIR:-/tmp}/neige-calm-mock-mutation.XXXXXX")
trap 'rm -rf "$temporary_root"' EXIT HUP INT TERM
cp tools/mock/generator.ts tools/mock/generator.test.ts tools/mock/fixture-manifest.mjs "$temporary_root/"
cp -R tools/mock/fixtures "$temporary_root/fixtures"
ln -s "$PWD/node_modules" "$temporary_root/node_modules"
implementation="$temporary_root/generator.ts"
baseline="$temporary_root/generator.baseline.ts"
cp "$implementation" "$baseline"

mutate() {
  name=$1 before=$2 after=$3 test_name=$4
  log=$(mktemp)
  cp "$baseline" "$implementation"
  BEFORE="$before" AFTER="$after" node -e 'const fs=require("fs");const p=process.argv[1];const s=fs.readFileSync(p,"utf8");if(!s.includes(process.env.BEFORE))process.exit(3);fs.writeFileSync(p,s.replace(process.env.BEFORE,process.env.AFTER))' "$implementation"
  if npx vitest --root "$temporary_root" run generator.test.ts --reporter=verbose >"$log" 2>&1; then
    echo "$name: SURVIVED" >&2; exit 1
  fi
  grep -F "$test_name" "$log" >/dev/null || { echo "$name: expected killer missing" >&2; exit 4; }
  echo "$name: killed by $test_name"
  rm -f "$log"
}

mutate leading-slash "path.startsWith('/')" "path.includes('/')" 'attributes every violation in no-leading-slash'
mutate unmatched-close "character === '}'" "character === '#'" 'attributes every violation in unmatched-close'
mutate missing-path-parameter 'for (const name of parsed.parameters)' 'for (const name of [])' 'reports both directions of path-parameter mismatch'
mutate declared-but-absent 'for (const name of declared)' 'for (const name of [])' 'reports both directions of path-parameter mismatch'
mutate required-responses 'if (!object(operation.responses)' 'if (false && !object(operation.responses)' 'attributes every violation in no-responses'
mutate stable-order 'Object.keys(value).sort()' 'Object.keys(value)' 'serializes object keys in stable code-point order'
mutate route-cardinality 'if (generatedRouteCount !== inputOperationCount)' 'if (false)' 'preserves operation/route cardinality'
mutate wire-types 'return new Set(Array.from(source.matchAll' 'return new Set(Array.from("".matchAll' 'deeply emits path parameters'
mutate parameter-emission 'parameters: parameters.map' 'parameters: [].map' 'deeply emits path parameters'
mutate response-ref "object(raw) && typeof raw.\$ref === 'string' ? resolveLocalRef(document, raw.\$ref) : raw" 'raw' 'deeply emits path parameters'
