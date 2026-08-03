#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
result="$(mktemp)"
backup="$(mktemp)"
trap 'rm -f "$result" "$backup"' EXIT

mutate() {
  local contract="$1" file="$2" expression="$3" test_file="$4"
  cp "$root/$file" "$backup"
  perl -0pi -e "$expression" "$root/$file"
  (cd "$root/fe" && npx vitest run "$test_file" --reporter=json --outputFile="$result" >/dev/null 2>&1) || true
  cp "$backup" "$root/$file"
  local failures
  failures="$(node -e 'const f=JSON.parse(require("node:fs").readFileSync(process.argv[1], "utf8")); process.stdout.write(String(f.numFailedTests))' "$result")"
  if [[ "$failures" != 1 ]]; then
    echo "FAIL|$contract|$failures"
    exit 1
  fi
  echo "PASS|$contract|1"
}

oracle="fe/tools/oracle/validator.ts"
mutate 'oracle required fields' "$oracle" 's/\x27family\x27, //' 'tools/oracle/oracle.test.ts'
mutate 'oracle enums' "$oracle" 's/\x27invariant\x27, \x27capability\x27, \x27gate\x27/\x27invariant\x27, \x27capability\x27, \x27gate\x27, \x27policy\x27/' 'tools/oracle/oracle.test.ts'
mutate 'oracle id format' "$oracle" 's/if \(!idMatch\) add/if (false \&\& !idMatch) add/' 'tools/oracle/oracle.test.ts'
mutate 'oracle id-kind agreement' "$oracle" 's/KIND_PREFIX\[String\(entry\.kind\)\] !== idMatch\[1\]/KIND_PREFIX[String(entry.kind)] !== idMatch[1] \&\& id !== \x27CAP-TEST-001\x27/' 'tools/oracle/oracle.test.ts'
mutate 'oracle id uniqueness' "$oracle" 's/if \(previous\) add/if (false \&\& previous) add/' 'tools/oracle/oracle.test.ts'
mutate 'oracle owner value domain' "$oracle" 's/!owners\.has\(entry\.owner_slice\)/false \&\& !owners.has(entry.owner_slice)/' 'tools/oracle/oracle.test.ts'
mutate 'oracle runtime owner prefix' "$oracle" 's/entry\.runtime_layer !== entry\.owner_slice\.split/false \&\& entry.runtime_layer !== entry.owner_slice.split/' 'tools/oracle/oracle.test.ts'
mutate 'oracle skipped reason' "$oracle" 's/if \(typeof entry\.skip_reason !== \x27string\x27 \|\| entry\.skip_reason\.trim\(\) === \x27\x27\)/if (false)/' 'tools/oracle/oracle.test.ts'
mutate 'oracle skipped null owner' "$oracle" 's/entry\.verification_owner !== null/entry.verification_owner !== null \&\& id !== \x27INV-TEST-001\x27/' 'tools/oracle/oracle.test.ts'
mutate 'oracle non-skipped reason ban' "$oracle" 's/if \(Object\.hasOwn\(entry, \x27skip_reason\x27\)\)/if (false \&\& Object.hasOwn(entry, \x27skip_reason\x27))/' 'tools/oracle/oracle.test.ts'
mutate 'oracle non-skipped owner' "$oracle" 's/if \(entry\.verification_owner === null\)/if (false \&\& entry.verification_owner === null)/' 'tools/oracle/oracle.test.ts'
mutate 'oracle intentional omission boolean' "$oracle" 's/typeof entry\.intentional_omission !== \x27boolean\x27/false/' 'tools/oracle/oracle.test.ts'
mutate 'oracle source locations' "$oracle" 's/if \(sourceErrors\.length\)/if (sourceErrors.length \&\& entry.source !== \x27fixture-source.txt:2\x27)/' 'tools/oracle/oracle.test.ts'
mutate 'oracle authoritative test locations' "$oracle" 's/if \(testErrors\.length\)/if (testErrors.length \&\& entry.authoritative_test !== \x27fixture-source.txt:2\x27)/' 'tools/oracle/oracle.test.ts'
mutate 'oracle why nonempty' "$oracle" 's/entry\.why\.trim\(\) === \x27\x27/entry.why.trim() === \x27\x27 \&\& entry.why !== \x27 \x27/' 'tools/oracle/oracle.test.ts'
mutate 'oracle statement nonempty' "$oracle" 's/entry\.statement\.trim\(\) === \x27\x27/false/' 'tools/oracle/oracle.test.ts'

styles="fe/tools/styles/audit.ts"
mutate 'CSS rule layering' "$styles" 's/if \(!layer\) violations/if (!layer \&\& rule.selector !== \x27.loose\x27) violations/' 'tools/styles/styles.test.ts'
mutate 'unlayered cm scope' "$styles" 's/if \(!classes\(rightmostCompound\(selector\)\)\.some/if (false \&\& !classes(rightmostCompound(selector)).some/' 'tools/styles/styles.test.ts'
mutate 'global class manifest equality' "$styles" 's/for \(const name of \[\.\.\.cssClasses\]/for (const name of []/' 'tools/styles/styles.test.ts'
mutate 'runtime style element audit' "$styles" 's/for \(const style of styleNodes\)/for (const style of styleNodes.filter(() => false))/' 'tools/styles/styles.test.ts'
mutate 'runtime stylesheet audit' "$styles" 's/for \(const sheet of Array\.from\(document\.styleSheets\)\)/for (const sheet of [])/' 'tools/styles/styles.test.ts'
mutate 'runtime inline audit' "$styles" 's/if \(\(element\.getAttribute/if (false \&\& (element.getAttribute/' 'tools/styles/styles.test.ts'

ownership="fe/tools/ownership/validator.ts"
mutate 'ownership exact paths only' "$ownership" 's/\x27\*\x27, \x27\?\x27, \x27\[\x27, \x27\]\x27/\x27?\x27, \x27[\x27, \x27]\x27/' 'tools/ownership/ownership.test.ts'
mutate 'ownership prefix conflicts' "$ownership" 's/if \(overlap\(entries\[left\], entries\[right\]\)\)/if (false \&\& overlap(entries[left], entries[right]))/' 'tools/ownership/ownership.test.ts'
mutate 'ownership current-tree coverage' "$ownership" 's/if \(count !== 1\)/if (false \&\& count !== 1)/' 'tools/ownership/ownership.test.ts'
mutate 'ownership readonly requests' "$ownership" 's/if \(!approved\)/if (false \&\& !approved)/' 'tools/ownership/ownership.test.ts'
