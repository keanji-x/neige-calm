// Settings › Planners with the production styles in a real browser (#1817): a long reason (a path in a
// fix hint) wraps inside its row at desktop and phone widths instead of widening the pane.
import { render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';

import '../../styles/entry.css';

import type { ProviderAvailability } from '../../../../core/domain/agent-providers.ts';
import { PlannersPane } from './planners.tsx';
import { SettingsSurface } from './public.tsx';

afterEach(() => { document.body.replaceChildren(); });

const LOGGED_OUT = 'not logged in — run `claude /login` with '
  + 'CLAUDE_CONFIG_DIR=/home/owner/.local/share/neige-next/claude-planner/config-dir-for-the-dedicated-login';

const PROVIDERS: readonly ProviderAvailability[] = [
  { provider: 'codex', status: 'ready', reason: null, checked_at_ms: 1_760_000_000_000 },
  { provider: 'claude', status: 'unavailable', reason: LOGGED_OUT, checked_at_ms: 1_760_000_000_000 },
];

describe('Settings Planners', () => {
  for (const [presentation, width, height] of [['desktop', 1180, 720], ['mobile-detail', 390, 844]] as const) {
    it(`shows each status and the whole reason without widening the ${presentation} pane`, async () => {
      await page.viewport(width, height);
      render(
        <SettingsSurface presentation={presentation} section="planners" onSelectSection={vi.fn()}>
          <PlannersPane providers={PROVIDERS} loadError={null} onRetryLoad={vi.fn()} onRecheck={vi.fn()}
            rechecking={false} recheckError={null} />
        </SettingsSurface>,
      );
      await expect.element(page.getByText(LOGGED_OUT)).toBeVisible();
      await expect.element(page.getByText('Unavailable')).toBeVisible();
      await expect.element(page.getByText('Ready')).toBeVisible();
      await expect.element(page.getByRole('button', { name: 'Recheck planners' })).toBeVisible();
      expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(width);
      const reason = page.getByText(LOGGED_OUT).element();
      expect(reason.getBoundingClientRect().right).toBeLessThanOrEqual(width);
      /* Whole, not cut to one line with an ellipsis: every box from the text up to its row fits its content. */
      for (let box: Element | null = reason; box !== null && box.tagName !== 'LI'; box = box.parentElement) {
        expect(box.scrollWidth, box.className).toBeLessThanOrEqual(box.clientWidth);
      }
      await page.screenshot({ path: `../../../../test-results/settings-planners-${presentation}.png` });
    });
  }
});
