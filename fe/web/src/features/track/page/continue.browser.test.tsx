import '../../../styles/entry.css';
import { cleanup } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import { renderPage, track } from './test-fixtures.tsx';

afterEach(cleanup);

for (const width of [390, 1440]) {
  it(`offers a readable, direct continuation of a closed track at ${width}px`, async () => {
    await page.viewport(width, 900);
    const onReply = vi.fn();
    const onReopenTrack = vi.fn(() => Promise.resolve());
    renderPage({ track: track({ closedAt: 5 }), canReopenTrack: true, onReopenTrack, onReply,
      report: <p>The saved result remains available.</p> });
    const button = page.getByRole('button', { name: '重新打开并继续' });
    await expect.element(button).toBeVisible();
    const bounds = button.element().getBoundingClientRect();
    expect(bounds.left).toBeGreaterThanOrEqual(0);
    expect(bounds.right).toBeLessThanOrEqual(width);
    await expect.element(page.getByText('The saved result remains available.')).toBeVisible();
    await button.click();
    expect(onReopenTrack).toHaveBeenCalledOnce();
    expect(onReply).toHaveBeenCalledOnce();
  });
}
