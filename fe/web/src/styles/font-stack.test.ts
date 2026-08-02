import { describe, expect, it } from 'vitest';

import { MONO_STACK } from './font-stack.js';

describe('MONO_STACK', () => {
  it('is directly consumable as one CSS font-family value', () => {
    expect(MONO_STACK).not.toContain('\n');
    expect(MONO_STACK.split(',').length).toBeGreaterThan(1);
  });
});
