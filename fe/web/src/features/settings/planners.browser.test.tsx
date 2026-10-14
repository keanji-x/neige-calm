// Settings › Planners with the production styles in a real browser (#1817): a long reason (a path in a
// fix hint) wraps inside its row at desktop and phone widths instead of widening the pane.
import { cleanup, render } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';

import '../../styles/entry.css';

import type { ProviderAvailability } from '../../../../core/domain/agent-providers.ts';
import { PlannersPane } from './planners.tsx';
import { SettingsSurface } from './public.tsx';

afterEach(() => { cleanup(); document.body.replaceChildren(); });

const LOGGED_OUT = 'not logged in — run `claude /login` with '
  + 'CLAUDE_CONFIG_DIR=/home/owner/.local/share/neige-next/claude-planner/config-dir-for-the-dedicated-login';

const PROVIDERS: readonly ProviderAvailability[] = [
  { provider: 'codex', status: 'ready', reason: null, authentication_notice: null, checked_at_ms: 1_760_000_000_000 },
  { provider: 'claude', status: 'unavailable', reason: LOGGED_OUT, authentication_notice: null, checked_at_ms: 1_760_000_000_000 },
];

describe('Settings Planners', () => {
  for (const [presentation, width, height] of [['desktop', 1180, 720], ['mobile-detail', 390, 844]] as const) {
    it(`shows each status and the whole reason without widening the ${presentation} pane`, async () => {
      await page.viewport(width, height);
      render(
        <SettingsSurface presentation={presentation} section="planners" onSelectSection={vi.fn()}>
          <PlannersPane turnDuration={{ readRevision: 1, value: undefined, loaded: true, loadError: null, onRetryLoad: vi.fn(), onSave: vi.fn().mockResolvedValue(undefined) }} authenticationRecovery={null} providers={PROVIDERS} loadError={null} onRetryLoad={vi.fn()} onRecheck={vi.fn()}
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


for (const width of [1180, 390]) {
  it(`edits minutes and resets the default at ${width}px`, async () => {
    await page.viewport(width, 844);
    const onSave = vi.fn().mockResolvedValue(undefined);
    render(<SettingsSurface presentation={width === 390 ? 'mobile-detail' : 'desktop'} section="planners" onSelectSection={vi.fn()}>
      <PlannersPane providers={[]} loadError={null} onRetryLoad={vi.fn()} onRecheck={vi.fn()}
        rechecking={false} recheckError={null} authenticationRecovery={null}
        turnDuration={{ readRevision: 1, value: undefined, loaded: true, loadError: null, onRetryLoad: vi.fn(), onSave }} />
    </SettingsSurface>);
    const input = page.getByRole('textbox', { name: 'Maximum turn duration (minutes)' });
    await expect.element(input).toHaveValue('30');
    await expect.element(input).toHaveAttribute('inputmode', 'numeric');
    await input.fill('45');
    await userEvent.keyboard('{Enter}');
    await expect.element(page.getByText('Saved.', { exact: true })).toBeVisible();
    expect(onSave).toHaveBeenCalledWith('45');
    await input.fill('241');
    await userEvent.keyboard('{Enter}');
    await expect.element(page.getByText('Enter a whole number from 1 to 240 minutes (ASCII digits).')).toHaveTextContent('1 to 240');
    await expect.element(input).toHaveValue('241');
    await expect.element(input).toHaveAttribute('aria-invalid', 'true');
    const described = input.element().getAttribute('aria-describedby');
    expect(described).toBeTruthy();
    const descriptions = described!.split(/\s+/).map((id) => document.getElementById(id));
    for (const element of descriptions) {
      expect(element).not.toBeNull();
      await expect.element(element!).toBeVisible();
    }
    const text = descriptions.map((element) => element!.textContent).join(' ');
    for (const phrase of ['Default: 30', '1–240', 'next turn', 'retrying an unconfirmed start keeps its frozen budget', 'Hold waiting', 'Backends without steer support', '1 to 240']) expect(text).toContain(phrase);
    expect(onSave).toHaveBeenCalledTimes(1);
    await input.fill('60');
    await expect.element(input).not.toHaveAttribute('aria-invalid', 'true');
    await userEvent.click(page.getByRole('button', { name: 'Restore default duration' }));
    expect(onSave).toHaveBeenLastCalledWith(null);
    await expect.element(input).toHaveValue('30');
    await expect.element(input).not.toHaveAttribute('aria-invalid', 'true');
    await expect.element(page.getByText(/next turn/)).toBeVisible();
    expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(width);
  });
}

it('marks a corrupt stored duration and associates its visible repair message', async () => {
  render(<PlannersPane providers={[]} loadError={null} onRetryLoad={vi.fn()} onRecheck={vi.fn()}
    rechecking={false} recheckError={null} authenticationRecovery={null}
    turnDuration={{ readRevision: 1, value: 'broken', loaded: true, loadError: null, onRetryLoad: vi.fn(), onSave: vi.fn().mockResolvedValue(undefined) }} />);
  const input = page.getByRole('textbox', { name: 'Maximum turn duration (minutes)' });
  await expect.element(input).toHaveAttribute('aria-invalid', 'true');
  const ids = input.element().getAttribute('aria-describedby');
  expect(ids).toBeTruthy();
  const elements = ids!.split(/\s+/).map((id) => document.getElementById(id));
  for (const element of elements) await expect.element(element!).toBeVisible();
  expect(elements.map((element) => element!.textContent).join(' ')).toContain('Stored duration is invalid');
  await userEvent.click(page.getByRole('button', { name: 'Restore default duration' }));
  await expect.element(input).toHaveValue('30');
  await expect.element(input).not.toHaveAttribute('aria-invalid', 'true');
});
