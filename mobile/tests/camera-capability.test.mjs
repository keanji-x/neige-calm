import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';

test('native camera capability is confined to the packaged Android launcher', async () => {
  const capability = JSON.parse(await readFile(new URL('../src-tauri/capabilities/launcher-camera.json', import.meta.url)));
  const config = JSON.parse(await readFile(new URL('../src-tauri/tauri.conf.json', import.meta.url)));
  assert.deepEqual(config.app.security.capabilities, ['launcher-camera', 'launcher-bundled-frontend']);
  assert.equal(capability.local, true);
  assert.equal(Object.hasOwn(capability, 'remote'), false);
  assert.deepEqual(capability.platforms, ['android']);
  assert.deepEqual(capability.windows, ['main']);
  assert.deepEqual(capability.permissions, [
    'barcode-scanner:allow-request-permissions',
    'barcode-scanner:allow-scan',
    'barcode-scanner:allow-cancel',
  ]);
});

test('server binding capability is confined to the packaged Android launcher', async () => {
  const capability = JSON.parse(await readFile(new URL('../src-tauri/capabilities/launcher-bundled-frontend.json', import.meta.url)));
  assert.equal(capability.local, true);
  assert.equal(Object.hasOwn(capability, 'remote'), false);
  assert.deepEqual(capability.windows, ['main']);
  assert.deepEqual(capability.platforms, ['android']);
  assert.deepEqual(capability.permissions, ['bundled-frontend:allow-bind-server', 'bundled-frontend:allow-login-tailscale', 'bundled-frontend:allow-connection-settings', 'bundled-frontend:allow-save-connection', 'bundled-frontend:allow-attempt-connection']);
});
