import { test } from 'node:test';
import assert from 'node:assert/strict';
import { nextUrl } from '../www/server-url.js';

test('default build rejects unconfigured HTTP origins and ports', () => {
  for (const input of ['http://192.0.2.20:4141', 'http://192.0.2.21:4140',
    'http://127.0.0.1:4141', 'http://example.com:4140', 'http://192.0.2.20.example.com:4140']) {
    assert.throws(() => nextUrl(input), Error, input);
  }
});

test('enters Next on the chosen server and preserves its port', () => {
  for (const path of ['', '/', '/next', '/next/']) {
    assert.equal(nextUrl(` https://calm.example.com:8443${path} `), 'https://calm.example.com:8443/next/');
  }
});

test('rejects non-HTTPS and credential-bearing destinations', () => {
  for (const input of ['http://example.com', 'javascript:alert(1)', 'file:///tmp/a',
    'https://user:secret@example.com', 'https://user@example.com', '//example.com', '']) {
    assert.throws(() => nextUrl(input), Error, input);
  }
});

test('rejects unexpected paths, query strings and fragments', () => {
  for (const suffix of ['/login', '/next/task/123', '/?token=secret', '/#token']) {
    assert.throws(() => nextUrl(`https://example.com${suffix}`), Error, suffix);
  }
});

test('rejects loopback and the bundled launcher origin', () => {
  for (const host of ['localhost', '127.0.0.1', '[::1]', 'tauri.localhost']) {
    assert.throws(() => nextUrl(`https://${host}`), Error, host);
  }
});
