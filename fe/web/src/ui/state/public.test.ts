import { beforeEach, describe, expect, it, vi } from 'vitest';

const reactHooks = vi.hoisted(() => ({
  useState: vi.fn(),
  useReducer: vi.fn(),
}));

vi.mock('react', () => reactHooks);

import { useReducer, useState } from './public.ts';

describe('ui/state runtime forwarding', () => {
  beforeEach(() => {
    reactHooks.useState.mockReset();
    reactHooks.useReducer.mockReset();
  });

  it('returns React useState output unchanged', () => {
    const reactResult = [{ count: 1 }, vi.fn()] as const;
    reactHooks.useState.mockReturnValue(reactResult);

    expect(useState({ count: 1 })).toBe(reactResult);
    expect(reactHooks.useState).toHaveBeenCalledWith({ count: 1 });
  });

  it('returns React useReducer output unchanged', () => {
    const reducer = (state: number, increment: number) => state + increment;
    const reactResult = [1, vi.fn()] as const;
    reactHooks.useReducer.mockReturnValue(reactResult);

    expect(useReducer(reducer, 1)).toBe(reactResult);
    expect(reactHooks.useReducer).toHaveBeenCalledWith(reducer, 1, undefined);
  });
});
