import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { resolve, join } from 'node:path';
import { readJUnit } from './junit.mjs';

if (process.argv.length < 4 || process.argv.length > 5) throw new Error('Usage: run.mjs <runtime.json> <api-level> [--mutation]');
const runtime = JSON.parse(await readFile(resolve(process.argv[2]), 'utf8'));
const api = Number(process.argv[3]);
const mutate = process.argv[4] === '--mutation';
assert.ok([26, 35].includes(api));
const android = fileURLToPath(new URL('../../src-tauri/gen/android/', import.meta.url));
for (const name of ['libapp_lib.so', 'libneige_p2p.so']) {
  assert.ok((await readFile(join(android, 'app/src/main/jniLibs/x86_64', name))).length > 0, `Build the exact native APK before backend instrumentation: ${name}`);
}
const reports = join(android, 'app/build/outputs/androidTest-results/connected');
const artifacts = fileURLToPath(new URL('../../artifacts/native/', import.meta.url));
await mkdir(artifacts, { recursive: true });
await rm(join(artifacts, 'mutation.json'), { force: true });
const args = [':app:connectedUniversalInstrumentedAndroidTest', '--max-workers=4', '--no-daemon',
  '-Pandroid.testInstrumentationRunnerArguments.class=io.neigecalm.next.BundledFrontendInstrumentationTest,io.neigecalm.next.OldWebViewInstrumentationTest',
  '-PabiList=x86_64', '-ParchList=x86_64', '-PtargetList=x86_64',
  `-Pandroid.testInstrumentationRunnerArguments.server_origin=${runtime.origin}`,
  `-Pandroid.testInstrumentationRunnerArguments.other_origin=${runtime.otherOrigin}`,
  `-Pandroid.testInstrumentationRunnerArguments.control_origin=${runtime.controlOrigin}`,
  `-Pandroid.testInstrumentationRunnerArguments.test_password=${runtime.password}`];
const remoteFence = 'io.neigecalm.next.BundledFrontendInstrumentationTest#remotePagesCannotRebindOrUseCamera';
const modern = [remoteFence,
  'io.neigecalm.next.BundledFrontendInstrumentationTest#localFrontendUsesRealBackendAndWebsocketWithoutAssetDownloads',
  'io.neigecalm.next.BundledFrontendInstrumentationTest#untrustedTlsEndpointCannotExecuteItsDocument'];
const old = 'io.neigecalm.next.OldWebViewInstrumentationTest#unsupportedWebViewShowsANativeUpgradeMessage';

async function run(label) {
  await rm(reports, { recursive: true, force: true });
  const result = spawnSync('./gradlew', args, { cwd: android, encoding: 'utf8', maxBuffer: 16 * 1024 * 1024 });
  const output = `${result.stdout ?? ''}${result.stderr ?? ''}`.replaceAll(runtime.password, '[fixture credential redacted]');
  await writeFile(join(artifacts, `${label}.log`), output);
  const report = readJUnit(reports);
  console.log(`${label}: process=${result.status}, cases=${JSON.stringify(report)}`);
  return { exit: result.status, report };
}

function green(result) {
  assert.equal(result.exit, 0, 'Native tests did not complete successfully; inspect the saved log');
  assert.equal(Object.keys(result.report).length, 4, 'Native discovery must match the selected backend and WebView tests');
  assert.ok(Object.values(result.report).every((value) => value !== 'failed'));
  if (api === 26 && result.report[old] === 'passed') return;
  for (const test of modern) assert.equal(result.report[test], 'passed', `Required native test did not run: ${test}`);
}

const baseline = await run('baseline');
green(baseline);
if (mutate) {
  assert.equal(api, 35);
  const source = join(android, 'app/src/main/java/io/neigecalm/next/BundledWebViewClient.kt');
  const original = await readFile(source, 'utf8');
  const needle = 'override fun onPageStarted(view: WebView, url: String, favicon: Bitmap?) = original.onPageStarted(view, url, favicon)';
  assert.equal(original.split(needle).length, 2);
  const mutated = original.replace(needle, 'override fun onPageStarted(view: WebView, url: String, favicon: Bitmap?) {}');
  let actual;
  try {
    await writeFile(source, mutated);
    assert.equal(await readFile(source, 'utf8'), mutated);
    const result = await run('missing-origin-delegation');
    actual = Object.entries(result.report).filter(([, value]) => value === 'failed').map(([name]) => name).sort();
    assert.notEqual(result.exit, 0);
    assert.deepEqual(actual, [remoteFence]);
    for (const test of modern.filter((name) => name !== remoteFence)) assert.equal(result.report[test], 'passed');
  } finally {
    await writeFile(source, original);
    assert.equal(await readFile(source, 'utf8'), original);
    green(await run('restored'));
  }
  await writeFile(join(artifacts, 'mutation.json'), JSON.stringify({ expected: [remoteFence], actual, restoredGreen: true,
    sourceSha256: createHash('sha256').update(original).digest('hex') }, null, 2));
}
