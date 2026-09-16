import { render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';

import '../../styles/entry.css';

import { MobileHeader } from './public.tsx';

afterEach(() => { document.body.replaceChildren(); });

const settlePaint = () => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));

describe('MobileHeader scroll surface', () => {
  it('keeps one flat title bar with the shared type scale while content scrolls beneath it', async () => {
    await page.viewport(390, 844);
    render(
      <div data-testid="scroll-host" style={{ blockSize: '200px', overflowY: 'auto', paddingInline: '16px' }}>
        <MobileHeader title="Report" backLabel="workspace" onBack={() => undefined} />
        <div style={{ blockSize: '800px' }}>Content</div>
      </div>,
    );

    const host = document.querySelector<HTMLElement>('[data-testid="scroll-host"]')!;
    const header = document.querySelector<HTMLElement>('[data-nc-mobile-header]')!;
    const top = header.getBoundingClientRect().top;
    expect(getComputedStyle(header).backdropFilter).toBe('none');

    host.scrollTop = 120;
    await settlePaint();

    expect(host.scrollTop).toBe(120);
    expect(getComputedStyle(header).backdropFilter).toBe('none');
    expect(getComputedStyle(header).borderRadius).toBe('0px');
    const title = await page.getByRole('heading', { name: 'Report', exact: true }).findElement();
    expect(getComputedStyle(title).fontSize).toBe('16px');
    const tone = document.createElement('span');
    tone.style.color = 'var(--text)';
    host.appendChild(tone);
    expect(getComputedStyle(title).color).toBe(getComputedStyle(tone).color);
    const back = await page.getByRole('button', { name: 'Back to workspace' }).findElement();
    expect(getComputedStyle(back).borderRadius).toBe('12px');
    expect(back.getBoundingClientRect().height).toBeGreaterThanOrEqual(44);
    expect(Math.abs(header.getBoundingClientRect().top - top)).toBeLessThan(1);
  });
});
