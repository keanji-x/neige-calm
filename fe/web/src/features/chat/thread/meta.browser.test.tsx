import '../../../styles/entry.css';
import { cleanup, render, screen, within } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import { ThreadStatusNotice } from './status-notice.tsx';

afterEach(async () => {
  cleanup(); document.documentElement.removeAttribute('data-theme');
  document.documentElement.style.removeProperty('--control-h');
  await page.viewport(1280, 720);
});

const cases = Object.freeze([
  { label: 'Running', detail: undefined, timestamp: null },
  { label: 'Completed', detail: undefined, timestamp: { kind: 'finished' as const, atMs: 1000 } },
  { label: 'Interrupted', detail: 'The response was interrupted.', timestamp: { kind: 'finished' as const, atMs: 1000 } },
  { label: 'Failed', detail: 'The model provider is temporarily unavailable.', timestamp: { kind: 'finished' as const, atMs: 1000 } },
  { label: 'Stopping', detail: 'Waiting for the response to end.', timestamp: null },
  { label: 'Paused', detail: 'The response is paused.', timestamp: { kind: 'paused' as const, atMs: 1000 } },
]);

for (const theme of ['light', 'dark'] as const) {
  for (const width of [320, 390, 1280]) {
    it(`shares token sizing and inline time behavior (${theme}, ${width}px)`, async () => {
      document.documentElement.dataset.theme = theme;
      await page.viewport(width, 844);
      for (const sample of cases) {
        cleanup();
        const { container } = render(<div style={{ width: Math.min(width - 32, 600) }}>
          <ThreadStatusNotice heading={sample.label} clock={{ elapsedMs: 27000, timestamp: sample.timestamp }}>
            {sample.detail === undefined ? undefined : <p>{sample.detail}</p>}
          </ThreadStatusNotice>
        </div>);
        const meta = screen.getByRole('status', { name: 'Current response status' });
        const label = screen.getByText(sample.label, { exact: true });
        const buttons = within(meta).getAllByRole('button', { name: /not available yet/ });
        expect(buttons).toHaveLength(3);
        expect(buttons.every((button) => button.hasAttribute('disabled') || button.getAttribute('aria-disabled') === 'true')).toBe(true);
        expect(getComputedStyle(label).fontSize).toBe('12px');
        expect(getComputedStyle(label).fontWeight).toBe('400');
        const center = (element: Element) => { const box = element.getBoundingClientRect(); return box.top + box.height / 2; };
        const check = () => {
          const icons = buttons.flatMap((button) => [...button.querySelectorAll('svg')]);
          const chevron = meta.querySelector('.astryx-icon');
          for (const icon of [...icons, ...(chevron === null ? [] : [chevron])]) {
            expect(Math.abs(center(icon) - center(label))).toBeLessThanOrEqual(1);
            // Rotation briefly enlarges a chevron's bounding rectangle; its layout box remains token-sized.
            expect(getComputedStyle(icon).width).toBe('12px');
          }
          expect(buttons.every((button) => button.getBoundingClientRect().height === 28)).toBe(true);
        };
        check();
        expect(meta.querySelector('[data-nc-meta-duration]')?.textContent).toContain('27s');
        await userEvent.hover(label);
        if (sample.timestamp !== null) {
          const time = meta.querySelector('time')!;
          expect(time.textContent).toMatch(/^\d{2}:\d{2}$/);
          expect(time.getAttribute('datetime')).toBe('1970-01-01T00:00:01.000Z');
          expect(meta.querySelector('[data-nc-meta-duration]')).toBeNull();
        } else expect(meta.querySelector('time')).toBeNull();
        check();
        if (sample.detail !== undefined) {
          const trigger = within(meta).getByRole('button', { expanded: false });
          trigger.focus(); await userEvent.keyboard('{Enter}');
          expect(trigger.getAttribute('aria-expanded')).toBe('true');
          expect(screen.getByText(sample.detail).checkVisibility()).toBe(true);
          check();
        }
        expect(container.querySelectorAll('[data-nc-current-meta]')).toHaveLength(1);
        expect(screen.queryByRole('tooltip')).toBeNull();
        expect(document.documentElement.scrollWidth).toBe(width);
      }
    });
  }
}

it('follows the system control token instead of the vendor fixed size', () => {
  document.documentElement.style.setProperty('--control-h', '32px');
  render(<ThreadStatusNotice heading="Completed" clock={{ elapsedMs: null, timestamp: null }} />);
  const meta = screen.getByRole('status', { name: 'Current response status' });
  for (const button of within(meta).getAllByRole('button')) expect(button.getBoundingClientRect().height).toBe(32);
});
