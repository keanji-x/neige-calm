import { page as browserPage } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';

import '../../../styles/entry.css';

import { renderPage, track } from './test-fixtures.tsx';

afterEach(() => { document.body.replaceChildren(); });

describe('the track lifecycle label in the page header', () => {
  it('sits directly beside the title as quiet label-sized text', async () => {
    await browserPage.viewport(1200, 800);
    renderPage({ track: track({ title: 'Status preview', lifecycle: 'working' }) });

    const title = document.querySelector<HTMLElement>('[aria-label="Rename track"]')!;
    const status = document.querySelector<HTMLElement>('[aria-label="Track lifecycle: Working"]')!;
    const gap = status.getBoundingClientRect().left - title.getBoundingClientRect().right;

    expect(gap).toBeGreaterThanOrEqual(4);
    expect(gap).toBeLessThanOrEqual(12);
    expect(getComputedStyle(status).fontSize).toBe('11px');
  });
});
