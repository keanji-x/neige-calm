import { describe, expect, it } from 'vitest';

import { asPersistent, createOverlayKey } from './types.js';

describe('Persistent runtime representation', () => {
  it('is the original value with no runtime wrapper', () => {
    const value = { count: 1 };

    expect(asPersistent(value)).toBe(value);
    expect(Object.keys(asPersistent(value))).toEqual(['count']);
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
