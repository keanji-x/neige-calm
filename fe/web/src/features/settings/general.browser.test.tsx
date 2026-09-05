import { render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';

import '../../styles/entry.css';

import { GeneralPane, SettingsSurface } from './public.tsx';

afterEach(() => { document.body.replaceChildren(); });

describe('Settings General desktop presentation', () => {
  it('renders the effective concurrency as a named numeric control', async () => {
    await page.viewport(1180, 720);
    render(
      <SettingsSurface section="general" onSelectSection={vi.fn()}>
        <GeneralPane
          settings={{ task_budget_default: '2' }}
          loadError={null}
          onSave={vi.fn()}
          onRetryLoad={vi.fn()}
        />
      </SettingsSurface>,
    );

    const concurrency = page.getByRole('spinbutton', { name: 'Task concurrency' });
    await expect.element(page.getByRole('button', { name: 'General' }))
      .toHaveAttribute('aria-current', 'page');
    await expect.element(concurrency).toBeInTheDocument();
    await expect.element(concurrency).toHaveValue(2);
    await page.screenshot({ path: '../../../../test-results/settings-general-desktop.png' });
  });
});
