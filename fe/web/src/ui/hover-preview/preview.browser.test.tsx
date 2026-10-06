import { cleanup, render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';

import '../../styles/entry.css';
import { HoverPreview } from './public.tsx';

afterEach(cleanup);
function mount() {
  return render(<div style={{ padding: 80 }}>
    <HoverPreview title="Long notes" trigger={(pin) => <button onClick={pin}>Long notes</button>}>
      <div>{Array.from({ length: 70 }, (_, i) => <p key={i}>Paragraph {i} — a long preview has its own scroll area.</p>)}</div>
    </HoverPreview>
    <div data-testid="drag-target" style={{ position: 'fixed', left: 700, top: 480, width: 30, height: 30 }}>Move here</div>
  </div>);
}

describe('preview in a real browser', () => {
  it('shows the ring, pins, scrolls independently, and moves with pointer capture', async () => {
    await page.viewport(1200, 900);
    mount();
    await page.getByRole('button', { name: 'Long notes', exact: true }).hover();
    await expect.poll(() => document.querySelector('[data-nc-link-preview]')).not.toBeNull();
    const card = document.querySelector<HTMLElement>('[data-nc-link-preview]')!;
    expect(card.querySelector('circle')).not.toBeNull();
    await expect.poll(() => card.hasAttribute('data-nc-pinned')).toBe(true);
    const body = card.children[1] as HTMLElement;
    expect(body.scrollHeight).toBeGreaterThan(body.clientHeight);
    await page.getByText('Paragraph 0 — a long preview has its own scroll area.').wheel({ delta: { y: 240 } });
    await expect.poll(() => body.scrollTop).toBeGreaterThan(0);
    const before = card.getBoundingClientRect();
    await page.getByRole('button', { name: 'Move preview' }).dropTo(page.getByTestId('drag-target'));
    const after = card.getBoundingClientRect();
    expect(after.left).toBeGreaterThan(before.left + 100);
    expect(after.top).toBeGreaterThan(before.top + 100);
    expect(after.right).toBeLessThanOrEqual(window.innerWidth - 11);
    await page.getByRole('button', { name: 'Close preview' }).click();
    await expect.poll(() => document.querySelector('[data-nc-link-preview]')).toBeNull();
  });
  it('allows keyboard movement and clamps a pinned card after viewport resize', async () => {
    await page.viewport(1200, 900); mount();
    await page.getByRole('button', { name: 'Long notes', exact: true }).click();
    const card = document.querySelector<HTMLElement>('[data-nc-link-preview]')!;
    const before = card.getBoundingClientRect().left;
    const move = page.getByRole('button', { name: 'Move preview' });
    (move.element() as HTMLElement).focus();
    await page.getByRole('button', { name: 'Move preview' }).click();
    // Native key events exercise the same focused title control as users.
    const { userEvent } = await import('vitest/browser');
    await userEvent.keyboard('{ArrowRight}');
    expect(card.getBoundingClientRect().left).toBe(before + 10);
    await page.viewport(390, 650);
    await expect.poll(() => card.getBoundingClientRect().right).toBeLessThanOrEqual(379);
    expect(card.getBoundingClientRect().left).toBeGreaterThanOrEqual(11);
    expect(card.getBoundingClientRect().bottom).toBeLessThanOrEqual(639);
    await userEvent.keyboard('{Escape}');
    await expect.poll(() => document.querySelector('[data-nc-link-preview]')).toBeNull();
  });
});
