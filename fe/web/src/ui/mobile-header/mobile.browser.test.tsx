import { render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';

import '../../styles/entry.css';

import { MobileHeader } from './public.tsx';

afterEach(() => { document.body.replaceChildren(); });

const settlePaint = () => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));

describe('MobileHeader scroll surface', () => {
  it('matches the page at rest and adds frost only after content scrolls beneath it', async () => {
    await page.viewport(390, 844);
    render(
      <div data-testid="scroll-host" style={{ blockSize: '200px', overflowY: 'auto', paddingInline: '16px' }}>
        <MobileHeader title="Report" />
        <div style={{ blockSize: '800px' }}>Content</div>
      </div>,
    );

    const host = document.querySelector<HTMLElement>('[data-testid="scroll-host"]')!;
    const header = document.querySelector<HTMLElement>('[data-nc-mobile-header]')!;
    const top = header.getBoundingClientRect().top;
    expect(header.hasAttribute('data-nc-mobile-scrolled')).toBe(false);
    expect(getComputedStyle(header).backdropFilter).toBe('none');

    host.scrollTop = 120;
    host.dispatchEvent(new Event('scroll'));
    await settlePaint();

    expect(header.hasAttribute('data-nc-mobile-scrolled')).toBe(true);
    expect(getComputedStyle(header).backdropFilter).toContain('blur');
    expect(Math.abs(header.getBoundingClientRect().top - top)).toBeLessThan(1);
  });
});
