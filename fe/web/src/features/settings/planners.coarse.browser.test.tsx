import { render, cleanup } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import '../../styles/entry.css';
import { PlannersPane } from './planners.tsx';
import { SettingsSurface } from './public.tsx';
afterEach(() => { cleanup(); document.body.replaceChildren(); });

for (const width of [390]) {
  it(`edits minutes and resets the default at ${width}px`, async () => {
    await page.viewport(width, 844);
    expect(matchMedia('(pointer: coarse)').matches).toBe(true);
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
    await expect.element(page.getByRole('alert')).toHaveTextContent('1 to 240');
    await expect.element(input).toHaveValue('241');
    expect(onSave).toHaveBeenCalledTimes(1);
    await userEvent.click(page.getByRole('button', { name: 'Restore default duration' }));
    expect(onSave).toHaveBeenLastCalledWith(null);
    await expect.element(page.getByText(/next turn/)).toBeVisible();
    expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(width);
  });
}
