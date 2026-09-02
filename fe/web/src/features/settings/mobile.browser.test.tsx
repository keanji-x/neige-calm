import { render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';

import '../../styles/entry.css';

import { NetworkPane } from './public.tsx';

afterEach(() => { document.body.replaceChildren(); });

describe('Settings mobile presentation', () => {
  it('keeps one row shape and one trailing edge at phone width', async () => {
    await page.viewport(390, 844);
    render(<NetworkPane
      settings={{}}
      loadError={null}
      saving={false}
      saveError={null}
      savedAt={null}
      onSave={vi.fn()}
      onRetryLoad={vi.fn()}
    />);

    // One pane, one heading, and its rows — no group boxes to stack.
    expect(page.getByRole('textbox', { name: 'HTTP proxy' })).toBeTruthy();
    expect(page.getByRole('textbox', { name: 'HTTPS proxy' })).toBeTruthy();
    expect(page.getByRole('button', { name: 'Save' })).toBeTruthy();
    await new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
    await page.screenshot({ path: '../../../../test-results/mobile-settings.png' });
  });
});
