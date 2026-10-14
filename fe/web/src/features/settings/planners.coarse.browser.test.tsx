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
    for (const phrase of ['Default: 30', '1–240', 'next turn', 'hold waiting', 'Claude receives only', '1 to 240']) expect(text).toContain(phrase);
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
