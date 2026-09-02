import { render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';

import '../../styles/entry.css';

import { NetworkPane } from './public.tsx';

afterEach(() => { document.body.replaceChildren(); });

describe('Settings mobile presentation', () => {
  it('keeps one row shape and one trailing edge at phone width', async () => {
    await page.viewport(390, 844);
    const { container } = render(<NetworkPane
      settings={{}}
      loadError={null}
      saving={false}
      saveError={null}
      savedAt={null}
      onSave={vi.fn()}
      onRetryLoad={vi.fn()}
    />);

    /*
     * `expect(locator).toBeTruthy()` — which these three assertions used to be
     * — cannot fail: a locator object is truthy whether or not it matches
     * anything. `expect.element` is the form that actually queries the page.
     */
    await expect.element(page.getByRole('textbox', { name: 'HTTP proxy' })).toBeInTheDocument();
    await expect.element(page.getByRole('textbox', { name: 'HTTPS proxy' })).toBeInTheDocument();
    // No Save button: a proxy commits when its field is left (see `public.tsx`).
    expect(container.querySelectorAll('button')).toHaveLength(0);
    await new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
    await page.screenshot({ path: '../../../../test-results/mobile-settings.png' });
  });
});
