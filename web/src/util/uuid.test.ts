// Covers the secure-context fallback path. The LAN-http bug (#72 follow-up)
// landed because nothing was exercising `makeUuid()` when `crypto.randomUUID`
// is `undefined` — only the polyfilled-jsdom happy path was tested.

import { afterEach, describe, expect, it } from 'vitest';
import { makeUuid } from './uuid';

const UUID_V4_REGEX =
  /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

// `randomUUID` lives on `Crypto.prototype` in jsdom, so a plain `delete`
// on the instance is a no-op. To force the fallback path we shadow it on
// the instance with `undefined` via `defineProperty`, then restore.
function withRandomUUIDDisabled<T>(fn: () => T): T {
  Object.defineProperty(globalThis.crypto, 'randomUUID', {
    configurable: true,
    value: undefined,
  });
  try {
    return fn();
  } finally {
    delete (globalThis.crypto as { randomUUID?: () => string }).randomUUID;
  }
}

describe('makeUuid', () => {
  const originalRandomUUID = (
    globalThis.crypto as Crypto & { randomUUID?: () => string }
  ).randomUUID;

  afterEach(() => {
    if (originalRandomUUID) {
      Object.defineProperty(globalThis.crypto, 'randomUUID', {
        configurable: true,
        value: originalRandomUUID,
      });
    } else {
      delete (globalThis.crypto as { randomUUID?: () => string }).randomUUID;
    }
  });

  it('delegates to crypto.randomUUID when present', () => {
    const sentinel = '11111111-2222-4333-8444-555555555555';
    Object.defineProperty(globalThis.crypto, 'randomUUID', {
      configurable: true,
      value: () => sentinel,
    });
    expect(makeUuid()).toBe(sentinel);
  });

  it('synthesises a v4 UUID matching the spec layout when randomUUID is absent', () => {
    withRandomUUIDDisabled(() => {
      const id = makeUuid();
      expect(id).toMatch(UUID_V4_REGEX);
    });
  });

  it('produces different values across calls in the fallback path', () => {
    withRandomUUIDDisabled(() => {
      const a = makeUuid();
      const b = makeUuid();
      expect(a).not.toBe(b);
      expect(a).toMatch(UUID_V4_REGEX);
      expect(b).toMatch(UUID_V4_REGEX);
    });
  });

  it('uses crypto.getRandomValues as its byte source in the fallback path', () => {
    // Deterministic byte stream: bytes[i] = i, so we get 0x00..0x0f.
    // After the version/variant bits are applied: byte[6] -> 0x40
    // (was 0x06; high nibble forced to 4) and byte[8] -> 0x88 (was 0x08;
    // top two bits forced to 10).
    const originalGetRandomValues = globalThis.crypto.getRandomValues;
    let callCount = 0;
    Object.defineProperty(globalThis.crypto, 'getRandomValues', {
      configurable: true,
      value: <T extends ArrayBufferView | null>(buf: T): T => {
        callCount++;
        const u8 = buf as unknown as Uint8Array;
        for (let i = 0; i < u8.length; i++) u8[i] = i;
        return buf;
      },
    });
    try {
      const id = withRandomUUIDDisabled(() => makeUuid());
      expect(callCount).toBe(1);
      expect(id).toBe('00010203-0405-4607-8809-0a0b0c0d0e0f');
      expect(id).toMatch(UUID_V4_REGEX);
    } finally {
      Object.defineProperty(globalThis.crypto, 'getRandomValues', {
        configurable: true,
        value: originalGetRandomValues.bind(globalThis.crypto),
      });
    }
  });
});
