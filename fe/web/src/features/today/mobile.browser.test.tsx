import { render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';

import '../../styles/entry.css';

import { TodayPage } from './public.tsx';

afterEach(() => { document.body.replaceChildren(); });

describe('Today mobile presentation', () => {
  /*
   * The four `not.toContain('Waiting on you' / 'Running' / 'Recent' /
   * 'Terminal')` assertions this used to carry are deleted on purpose
   * (#1191 §5). Today's mobile presentation is missing those sections, and
   * asserting their *absence* made restoring them a test failure — a mine laid
   * for whoever does B2. What the phone width has to keep proving is that the
   * calendar surface renders and is a grid; that it is currently the only
   * thing there is a fact about the backlog, not a contract.
   */
  it('renders the Astryx calendar surface at phone width', async () => {
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
    expect(container.querySelector('[role="grid"]')).not.toBeNull();
    await new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
    await page.screenshot({ path: '../../../../test-results/mobile-today.png' });
  });
});
