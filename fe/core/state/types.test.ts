import { describe, expect, it } from 'vitest';

import { createOverlayKey, unsafeAsPersistent } from './types.js';

describe('Persistent runtime representation', () => {
  it('is the original value with no runtime wrapper', () => {
    const value = { count: 1 };

    expect(unsafeAsPersistent(value)).toBe(value);
    expect(Object.keys(unsafeAsPersistent(value))).toEqual(['count']);
  });
});

describe('overlay key', () => {
  it('keeps the five-part family shape used by persistence and invalidation', () => {
    expect(createOverlayKey('plugin', 'entity', '42', 'layout')).toEqual([
      'overlay',
      'plugin',
      'entity',
      '42',
      'layout',
    ]);
  });
});
