import { render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';

import '../../styles/entry.css';

import { SettingsPage } from './public.tsx';

afterEach(() => { document.body.replaceChildren(); });

describe('Settings mobile presentation', () => {
  it('uses Astryx cards and standard form controls', async () => {
    await page.viewport(390, 844);
    const { container } = render(<SettingsPage
      settings={{}}
      loadError={null}
      saving={false}
      saveError={null}
      savedAt={null}
      onSave={vi.fn()}
      onRetryLoad={vi.fn()}
      onOpenToday={vi.fn()}
      themeMode="system"
      onThemeModeChange={vi.fn()}
      onOpenTemplates={vi.fn()}
    />);

    // Network, Templates, Appearance, About. #1230 added Templates; the count
    // is asserted rather than `toBeGreaterThan` so adding a section is a
    // deliberate edit here, not something that slips in unnoticed.
    expect(container.querySelectorAll('[data-nc-settings-card]')).toHaveLength(4);
    expect(page.getByRole('textbox', { name: 'HTTP proxy' })).toBeTruthy();
    expect(page.getByRole('textbox', { name: 'HTTPS proxy' })).toBeTruthy();
    expect(page.getByRole('radiogroup', { name: 'Appearance' })).toBeTruthy();
    expect(page.getByRole('button', { name: 'Edit templates' })).toBeTruthy();
    expect(page.getByRole('button', { name: 'Save' })).toBeTruthy();
    await new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
    await page.screenshot({ path: '../../../../test-results/mobile-settings.png' });
  });
});
