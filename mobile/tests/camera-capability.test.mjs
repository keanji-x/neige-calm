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
  assert.deepEqual(capability.permissions, ['bundled-frontend:allow-bind-server', 'bundled-frontend:allow-enroll-from-scan', 'bundled-frontend:allow-cancel-enrollment', 'bundled-frontend:allow-reset-enrollment', 'bundled-frontend:allow-connection-settings', 'bundled-frontend:allow-save-connection', 'bundled-frontend:allow-select-saved-tailnet', 'bundled-frontend:allow-attempt-connection', 'bundled-frontend:allow-confirm-legacy-tailnet']);
});

test('scan enrollment replaces every reachable phone browser-login command and JNI entry', async () => {
  const paths = ['www/app.js', 'src-tauri/build.rs', 'p2p-native/main.go', 'p2p-native/bridge_android.c',
    'src-tauri/gen/android/app/src/main/java/io/neigecalm/next/NativeP2P.kt',
    'src-tauri/gen/android/app/src/main/java/io/neigecalm/next/BundledFrontendPlugin.kt'];
  const sources = await Promise.all(paths.map(path => readFile(new URL(`../${path}`, import.meta.url), 'utf8')));
  for (let i = 0; i < sources.length; i++) assert.doesNotMatch(sources[i],
    /login_tailscale|loginTailscale|p2pLogin|NativeP2P_login|NativeP2P\.login|external fun login|ACTION_VIEW|startActivity\(/, paths[i]);
  assert.match(sources[1], /"enroll_from_scan"/);
  assert.match(sources[2], /\/\/export p2pEnroll/);
  assert.match(sources[3], /Java_io_neigecalm_next_NativeP2P_enroll/);
  assert.match(sources[4], /external fun enroll/);
  assert.match(sources[5], /@Command fun enrollFromScan/);
});
