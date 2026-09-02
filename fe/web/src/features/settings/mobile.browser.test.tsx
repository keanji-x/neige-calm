import { render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';

import '../../styles/entry.css';

import { SettingsPage } from './public.tsx';

afterEach(() => { document.body.replaceChildren(); });

describe('Settings mobile presentation', () => {
  it('uses astryx form controls and one group per settings heading', async () => {
    await page.viewport(390, 844);
    const { container } = render(<SettingsPage
      settings={{}}
      loadError={null}
      saving={false}
      saveError={null}
      savedAt={null}
      onSave={vi.fn()}
      onRetryLoad={vi.fn()}
      themeMode="system"
      onThemeModeChange={vi.fn()}
    />);

    // Network, Appearance, About — three groups, asserted by count rather than
    // `toBeGreaterThan` so adding one is a deliberate edit here. They are
    // headings and hairlines now, not cards: a card is a boundary, and these
    // are three parts of one screen.
    expect(container.querySelectorAll('section[aria-labelledby^="nc-settings-"]')).toHaveLength(3);
    expect(page.getByRole('textbox', { name: 'HTTP proxy' })).toBeTruthy();
    expect(page.getByRole('textbox', { name: 'HTTPS proxy' })).toBeTruthy();
    expect(page.getByRole('radiogroup', { name: 'Theme' })).toBeTruthy();
    expect(page.getByRole('button', { name: 'Save' })).toBeTruthy();
    await new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
    await page.screenshot({ path: '../../../../test-results/mobile-settings.png' });
  });
});
