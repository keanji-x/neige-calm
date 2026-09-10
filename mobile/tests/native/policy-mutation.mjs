import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';
import { readJUnit } from './junit.mjs';

const android = fileURLToPath(new URL('../../src-tauri/gen/android/', import.meta.url));
const source = join(android, 'app/src/main/java/io/neigecalm/next/BundledOrigin.kt');
const reports = join(android, 'app/build/test-results/testUniversalDebugUnitTest');
const artifacts = fileURLToPath(new URL('../../artifacts/native-policy/', import.meta.url));
await mkdir(artifacts, { recursive: true });
await rm(join(artifacts, 'mutation.json'), { force: true });
const original = await readFile(source, 'utf8');
const needle = '      require(!reservedHost(origin.host) || (configuredHttp && !launcher)) { "The server cannot use a privileged or unconfigured local origin" }\n';
assert.equal(original.split(needle).length, 2);
// The first test also pins that an explicit HTTP exception never permits the
// privileged launcher; the second pins HTTPS/local-address rejection.
const expected = ['io.neigecalm.next.BundledOriginTest#originRequiresAnExplicitSafeAuthority',
  'io.neigecalm.next.BundledOriginTest#serverBindingCannotTargetPrivilegedOrLoopbackOrigins'];

async function run(label) {
  await rm(reports, { recursive: true, force: true });
  const result = spawnSync('./gradlew', [':app:testUniversalDebugUnitTest', '--tests', 'io.neigecalm.next.BundledOriginTest', '--tests', 'io.neigecalm.next.ConnectionAttemptTest',
    '--max-workers=4', '--no-daemon'], { cwd: android, encoding: 'utf8', maxBuffer: 16 * 1024 * 1024 });
  await writeFile(join(artifacts, `${label}.log`), `${result.stdout ?? ''}${result.stderr ?? ''}`);
  const report = readJUnit(reports);
  assert.equal(Object.keys(report).filter((name) => name.startsWith('io.neigecalm.next.BundledOriginTest#')).length, 5, 'Origin policy tests did not all run');
  assert.equal(Object.keys(report).filter((name) => name.startsWith('io.neigecalm.next.ConnectionAttemptTest#')).length, 6, 'Direct-route policy tests did not all run');
  console.log(`${label}: ${JSON.stringify(report)}`);
  return { exit: result.status, report };
}
function green(result) {
  assert.equal(result.exit, 0);
  assert.ok(Object.values(result.report).every((status) => status === 'passed'));
}
green(await run('baseline'));
let actual;
try {
  const mutated = original.replace(needle, '');
  await writeFile(source, mutated);
  assert.equal(await readFile(source, 'utf8'), mutated);
  const result = await run('without-reserved-origin-fence');
  actual = Object.entries(result.report).filter(([, state]) => state === 'failed').map(([name]) => name).sort();
  assert.notEqual(result.exit, 0);
  assert.deepEqual(actual, expected);
  assert.ok(Object.values(result.report).every((state) => state === 'passed' || state === 'failed'));
} finally {
  await writeFile(source, original);
  assert.equal(await readFile(source, 'utf8'), original);
  green(await run('restored'));
}
await writeFile(join(artifacts, 'mutation.json'), JSON.stringify({ expected, actual, restoredGreen: true,
  sourceSha256: createHash('sha256').update(original).digest('hex') }, null, 2));
