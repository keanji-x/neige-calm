import { test } from 'node:test';
import assert from 'node:assert/strict';
import { pairingDestination } from '../www/pairing-url.js';

test('accepts a versioned HTTPS QR without requiring a phone VPN', () => {
  const url = `https://pair.example.ts.net:10000/mobile/pair#v1.${'a'.repeat(64)}`;
  assert.deepEqual(pairingDestination(url), { url, origin: 'https://pair.example.ts.net:10000', host: 'pair.example.ts.net:10000' });
});

test('rejects unsafe, malformed or credential-bearing QR destinations', () => {
  const token = 'a'.repeat(64);
  for (const url of [
    `http://pair.example.ts.net/mobile/pair#v1.${token}`,
    `https://user:password@pair.example.ts.net/mobile/pair#v1.${token}`,
    `https://pair.example.ts.net/mobile/pair?ticket=${token}`,
    `https://pair.example.ts.net/next/#v1.${token}`,
    `https://pair.example.ts.net/mobile/pair#v2.${token}`,
    `https://localhost/mobile/pair#v1.${token}`, 'javascript:alert(1)', '{}',
  ]) assert.throws(() => pairingDestination(url), Error, url);
});
