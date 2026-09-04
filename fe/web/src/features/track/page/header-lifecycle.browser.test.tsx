import { page as browserPage, userEvent } from 'vitest/browser';
import { render } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import '../../../styles/entry.css';

import { useState } from '../../../ui/state/public.ts';
import { TrackLifecycleBadge } from '../lifecycle-badge/public.tsx';
import { renderPage, track } from './test-fixtures.tsx';

afterEach(() => {
  document.body.replaceChildren();
  delete document.documentElement.dataset.theme;
});

type Rgb = readonly [number, number, number];

function paintedRgb(cssColor: string): Rgb {
  const canvas = document.createElement('canvas');
  canvas.width = 1;
  canvas.height = 1;
  const context = canvas.getContext('2d', { willReadFrequently: true });
  if (context === null) throw new Error('no 2d canvas context');
  context.fillStyle = cssColor;
  context.fillRect(0, 0, 1, 1);
  const [r, g, b] = context.getImageData(0, 0, 1, 1).data;
  return [r, g, b];
}

function contrast(first: Rgb, second: Rgb): number {
  const channel = (byte: number) => {
    const value = byte / 255;
    return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
  };
  const luminance = ([r, g, b]: Rgb) =>
    0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
  const [high, low] = [luminance(first), luminance(second)].sort((a, b) => b - a);
  return (high + 0.05) / (low + 0.05);
}

describe('the track lifecycle control in the page header', () => {
  it('sits directly beside the title as a quiet compact button', async () => {
    await browserPage.viewport(1200, 800);
    renderPage({ track: track({ title: 'Status preview', lifecycle: 'working' }) });

    const title = document.querySelector<HTMLElement>('[aria-label="Rename track"]')!;
    const status = document.querySelector<HTMLElement>('[aria-label="Track lifecycle: Working"]')!;
    const gap = status.getBoundingClientRect().left - title.getBoundingClientRect().right;

    expect(gap).toBeGreaterThanOrEqual(4);
    expect(gap).toBeLessThanOrEqual(12);
    expect(getComputedStyle(status).fontSize).toBe('11px');
    expect(title.scrollWidth).toBeLessThanOrEqual(title.clientWidth);
  });

  it('borrows the side-panel card surface and radius without a border', async () => {
    await browserPage.viewport(1200, 800);
    renderPage({ track: track({ lifecycle: 'done' }), canResumeTrack: true });

    const status = document.querySelector<HTMLElement>('[aria-label="Track lifecycle: Done"]')!;
    const panelCard = document.querySelector<HTMLElement>('[data-nc-desktop-panel] > div')!;
    const statusStyle = getComputedStyle(status);
    const panelStyle = getComputedStyle(panelCard);

    expect(paintedRgb(statusStyle.backgroundColor)).toEqual(paintedRgb(panelStyle.backgroundColor));
    expect(statusStyle.borderRadius).toBe(panelStyle.borderRadius);
    expect(statusStyle.borderTopWidth).toBe('0px');
  });

  it('returns focus to the lifecycle button when Escape closes its menu', async () => {
    await browserPage.viewport(1200, 800);
    renderPage({ track: track({ lifecycle: 'done' }), canResumeTrack: true });
    const status = document.querySelector<HTMLButtonElement>('[aria-label="Track lifecycle: Done"]')!;

    status.click();
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
    expect(document.querySelector<HTMLElement>('[role="menu"]')?.closest('[popover]')
      ?.matches(':popover-open')).toBe(true);

    await userEvent.keyboard('{Escape}');
    expect(document.activeElement).toBe(status);
  });

  it('keeps keyboard focus on the lifecycle fact after Resume removes the action', async () => {
    function ResumeHarness() {
      const [resumed, setResumed] = useState(false);
      return (
        <TrackLifecycleBadge
          lifecycle={resumed ? 'working' : 'done'}
          canResume={!resumed}
          onResume={() => { setResumed(true); }}
        />
      );
    }

    await browserPage.viewport(1200, 800);
    render(<ResumeHarness />);
    const status = document.querySelector<HTMLButtonElement>('[aria-label="Track lifecycle: Done"]')!;
    status.focus();
    await userEvent.keyboard('{Enter}');
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
    expect(document.activeElement?.getAttribute('role')).toBe('menuitem');

    await userEvent.keyboard('{Enter}');
    const lifecycle = document.querySelector<HTMLElement>('[data-testid="track-lifecycle"]')!;
    expect(document.activeElement).toBe(lifecycle);
    expect(lifecycle.matches(':focus-visible')).toBe(true);
    expect(getComputedStyle(lifecycle).outlineStyle).not.toBe('none');
    expect(document.querySelector('[aria-label="Track lifecycle: Working"]')).not.toBeNull();
  });

  it('light-dismisses the lifecycle menu on an outside click', async () => {
    await browserPage.viewport(1200, 800);
    renderPage({ track: track({ lifecycle: 'done' }), canResumeTrack: true });
    const status = document.querySelector<HTMLButtonElement>('[aria-label="Track lifecycle: Done"]')!;
    const popover = () => document.querySelector<HTMLElement>('[role="menu"]')?.closest<HTMLElement>('[popover]');

    status.click();
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
    expect(popover()?.matches(':popover-open')).toBe(true);
    await userEvent.click(document.body);
    expect(popover()?.matches(':popover-open')).toBe(false);
  });

  it('truncates a long title before the label can overlap trailing actions', async () => {
    /* Stay just above the 60rem mobile navigation cutover: below it the
       desktop PageHeader is intentionally replaced by MobileHeader. */
    await browserPage.viewport(1000, 600);
    renderPage({
      track: track({ title: 'A deliberately long track title '.repeat(12), lifecycle: 'working' }),
    });

    const title = document.querySelector<HTMLElement>('[aria-label="Rename track"]')!;
    const status = document.querySelector<HTMLElement>('[aria-label="Track lifecycle: Working"]')!;
    const remove = document.querySelector<HTMLElement>('[aria-label^="Delete track "]')!;
    const titleBox = title.getBoundingClientRect();
    const statusBox = status.getBoundingClientRect();

    expect(title.scrollWidth).toBeGreaterThan(title.clientWidth);
    expect(statusBox.left - titleBox.right).toBeGreaterThanOrEqual(4);
    expect(statusBox.left - titleBox.right).toBeLessThanOrEqual(12);
    expect(statusBox.right).toBeLessThan(remove.getBoundingClientRect().left);
  });

  it('keeps the subdued running colour readable in both themes', async () => {
    await browserPage.viewport(1200, 800);
    renderPage({ track: track({ lifecycle: 'working' }) });
    const status = document.querySelector<HTMLElement>('[aria-label="Track lifecycle: Working"]')!;

    for (const theme of ['light', 'dark'] as const) {
      document.documentElement.dataset.theme = theme;
      const foreground = paintedRgb(getComputedStyle(status).color);
      const background = paintedRgb(getComputedStyle(status).backgroundColor);
      expect(contrast(foreground, background), theme).toBeGreaterThanOrEqual(4.5);
    }
  });
});
