import { cleanup } from '@testing-library/react';
import { page as browserPage, userEvent } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import '../../../styles/entry.css';
import { renderPage, track } from './test-fixtures.tsx';
afterEach(() => { cleanup(); document.body.replaceChildren(); });

for (const width of [390]) {
  it(`keeps timeout text and open/dismiss controls visible with none attention at ${width}px`, async () => {
    await browserPage.viewport(width, 844);
    expect(matchMedia('(pointer: coarse)').matches).toBe(true);
    const onReply = vi.fn();
    const onDismiss = vi.fn().mockResolvedValue(undefined);
    renderPage({ track: track({ attention: 'none', working: false }), onReply, onDismiss,
      inputNotifications: [{ key: 'planner_turn_timeout:9', kind: 'planner-turn-timeout', atMs: Date.UTC(2026, 9, 14, 12),
        text: 'Turn stopped at 2026-10-14 12:00 UTC. Execution: 30 minutes; hold time excluded.' }] });
    await expect.element(browserPage.getByText('Planner turn timed out')).toBeVisible();
    await expect.element(browserPage.getByText(/Turn stopped at/)).toBeVisible();
    await expect.element(browserPage.getByText('2026-10-14T12:00:00.000Z', { exact: true })).toBeVisible();
    await userEvent.click(browserPage.getByRole('button', { name: /^Open the Planner:/ }));
    expect(onReply).toHaveBeenCalledTimes(1);
    await userEvent.click(browserPage.getByRole('button', { name: /^Dismiss: Planner turn timed out:/ }));
    expect(onDismiss).toHaveBeenCalledWith('planner_turn_timeout:9');
    expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(width);
    await browserPage.screenshot({ path: `../../../../../test-results/timeout-${width}-${matchMedia('(pointer: coarse)').matches ? 'coarse' : 'desktop'}.png` });
  });
}
