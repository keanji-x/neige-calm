#!/usr/bin/env node
// Exercise the actual preview driver while injecting cleanup dependency failures.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { createInterface } from 'node:readline';
import { parseArgs } from 'node:util';
import { resolve } from 'node:path';
import { closePreview, deadline, terminateDriver } from './terminal-preview-cleanup.mjs';

const { values } = parseArgs({ options: { driver: { type: 'string' }, runtime: { type: 'string' } } });
assert.ok(values.driver && values.runtime, '--driver BIN --runtime BIN required');
async function start() {
  const child = spawn(resolve(values.driver), ['--runtime-executable', resolve(values.runtime),
    '--cwd', '/tmp', '--program', '/bin/sh', '--', '-c', "printf 'ready\\n'; read line"],
  { stdio: ['pipe', 'pipe', 'pipe'], env: { PATH: '/usr/bin:/bin', LANG: 'C.UTF-8' } });
  let stderr = '';
  child.stderr.on('data', bytes => { stderr = (stderr + bytes).slice(-8192); });
  const exited = once(child, 'exit');
  void exited.catch(() => {});
  const lines = createInterface({ input: child.stdout });
  try {
    const [line] = await deadline(once(lines, 'line'), 'driver startup timeout');
    assert.deepEqual(JSON.parse(line), { ready: true });
    return { child, exited, stderr: () => stderr, lines };
  } catch (error) {
    child.stdin.end(); await terminateDriver(child, exited); lines.close(); throw error;
  }
}
for (const failure of ['browser', 'drain', 'signal']) {
  const fixture = await start();
  try {
    if (failure === 'signal') {
      const [code] = await terminateDriver(fixture.child, fixture.exited);
      assert.equal(code, 1);
      assert.match(fixture.stderr(), /preview parent terminated/,
        'driver must finish graceful runtime cleanup before reporting parent cancellation');
    } else {
      await assert.rejects(closePreview({
        browser: { close: async () => { if (failure === 'browser') throw new Error('injected browser close failure'); } },
        driver: fixture.child, exited: fixture.exited, stderr: fixture.stderr,
        pending: failure === 'drain' ? new Promise(() => {}) : Promise.resolve(), drainMs: 10,
      }), error => error instanceof AggregateError && error.errors.some(cause =>
        cause.message.includes(failure === 'browser' ? 'injected browser close failure' : 'input drain timeout')));
      assert.notEqual(fixture.child.exitCode, null, 'cleanup must reap the driver despite another failure');
    }
  } finally {
    fixture.child.stdin.end();
    await terminateDriver(fixture.child, fixture.exited);
    fixture.lines.close();
  }
}
console.log('PASS: browser-close error, input-drain timeout and parent TERM all reap the real preview driver');
