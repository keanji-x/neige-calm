import { afterEach, describe, expect, it, vi } from 'vitest';

import { osc52HostMayWrite } from './osc52.ts';

describe('osc52HostMayWrite', () => {
  afterEach(() => {
    document.body.replaceChildren();
    vi.restoreAllMocks();
  });

  it('refuses a hidden card even when the tab is focused on body', () => {
    vi.spyOn(document, 'hasFocus').mockReturnValue(true);
    const host = document.createElement('div');
    document.body.append(host);
    expect(osc52HostMayWrite(host, true)).toBe(true);
    expect(osc52HostMayWrite(host, false)).toBe(false);
  });
});
