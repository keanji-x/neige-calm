import { render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';

import '../../styles/entry.css';

import { TodayPage } from './public.tsx';

afterEach(() => { document.body.replaceChildren(); });

describe('Today mobile presentation', () => {
  it('renders only the Astryx calendar surface', async () => {
    await page.viewport(390, 844);
    const { container } = render(
      <TodayPage
        waves={[]}
        coves={[]}
        nowMs={Date.UTC(2026, 7, 31, 9, 30)}
        renderWaveRow={() => null}
      />,
    );

    expect(page.getByRole('heading', { name: 'Today' })).toBeTruthy();
    expect(container.textContent).not.toContain('Waiting on you');
    expect(container.textContent).not.toContain('Running');
    expect(container.textContent).not.toContain('Recent');
    expect(container.textContent).not.toContain('Terminal');
    expect(container.querySelector('[role="grid"]')).not.toBeNull();
    await new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
    await page.screenshot({ path: '../../../../test-results/mobile-today.png' });
  });
});
