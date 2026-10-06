import { cleanup, render } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';

import '../../styles/entry.css';
import { HoverPreview } from './public.tsx';

afterEach(cleanup);
function mount(onRead = () => {}) {
  return render(<div style={{ padding: 80 }}>
    <HoverPreview title="Long notes" trigger={(activate) => <button onClick={activate}>Long notes</button>}>
      <button type="button" onClick={onRead}>Continue reading</button>
      <div>{Array.from({ length: 70 }, (_, i) => <p key={i}>Paragraph {i} — a long preview has its own scroll area.</p>)}</div>
    </HoverPreview>
    <button type="button" data-testid="outside" style={{ position: 'fixed', left: 800, top: 80 }}>Elsewhere</button>
  </div>);
}

describe('preview in a real browser', () => {
  it('keeps the link-to-card edge open during slow transfer, supports interaction, then dismisses', async () => {
    await page.viewport(1200, 900);
    const onRead = vi.fn(); mount(onRead);
    await page.getByRole('button', { name: 'Long notes', exact: true }).hover();
    await expect.poll(() => document.querySelector('[data-nc-link-preview]')).not.toBeNull();
    const card = document.querySelector<HTMLElement>('[data-nc-link-preview]')!;
    expect(card.querySelector('circle')).not.toBeNull();
    await expect.poll(() => card.hasAttribute('data-nc-ready')).toBe(true);
    const before = performance.now();
    await page.getByRole('dialog').hover({ position: { x: 30, y: -6 }, force: true });
    await expect.poll(() => performance.now() - before).toBeGreaterThan(350);
    expect(card.isConnected).toBe(true);
    await page.getByRole('button', { name: 'Continue reading' }).click();
    expect(onRead).toHaveBeenCalledOnce();
    const body = card.children[1] as HTMLElement;
    expect(body.scrollHeight).toBeGreaterThan(body.clientHeight);
    await page.getByText('Paragraph 0 — a long preview has its own scroll area.').wheel({ delta: { y: 240 } });
    await expect.poll(() => body.scrollTop).toBeGreaterThan(0);
    await page.getByTestId('outside').hover();
    await expect.poll(() => document.querySelector('[data-nc-link-preview]')).toBeNull();
  });
  it('hands keyboard focus into the preview and dismisses after native Tab leaves', async () => {
    await page.viewport(1200, 900);
    const onRead = vi.fn(); mount(onRead);
    await userEvent.tab();
    expect(document.activeElement).toBe(page.getByRole('button', { name: 'Long notes', exact: true }).element());
    await userEvent.keyboard('{ArrowDown}');
    const content = page.getByRole('region', { name: 'Preview content: Long notes' });
    await expect.poll(() => document.activeElement).toBe(content.element());
    await userEvent.tab();
    expect(document.activeElement).toBe(page.getByRole('button', { name: 'Continue reading' }).element());
    await userEvent.keyboard('{Enter}');
    expect(onRead).toHaveBeenCalledOnce();
    const before = performance.now();
    await expect.poll(() => performance.now() - before).toBeGreaterThan(350);
    expect(document.querySelector('[data-nc-link-preview]')).not.toBeNull();
    for (let step = 0; step < 6 && document.activeElement !== page.getByTestId('outside').element(); step += 1) {
      await userEvent.tab({ shift: true });
    }
    expect(document.activeElement).toBe(page.getByTestId('outside').element());
    await expect.poll(() => document.querySelector('[data-nc-link-preview]')).toBeNull();
  });
  it('keeps both cards open while scrolling a nested portal preview', async () => {
    await page.viewport(1200, 900);
    render(<div style={{ padding: 80 }}>
      <HoverPreview title="Parent" trigger={(activate) => <button onClick={activate}>Parent</button>}>
        <HoverPreview title="Child" trigger={(activate) => <button onClick={activate}>Child</button>}>
          <div>{Array.from({ length: 70 }, (_, i) => <p key={i}>Nested paragraph {i}</p>)}</div>
        </HoverPreview>
      </HoverPreview>
    </div>);
    await page.getByRole('button', { name: 'Parent', exact: true }).click();
    await page.getByRole('button', { name: 'Child', exact: true }).click();
    const child = page.getByRole('dialog', { name: 'Preview: Child' });
    const body = child.element().children[1] as HTMLElement;
    await child.getByText('Nested paragraph 0', { exact: true }).wheel({ delta: { y: 240 } });
    await expect.poll(() => body.scrollTop).toBeGreaterThan(0);
    const before = performance.now();
    await expect.poll(() => performance.now() - before).toBeGreaterThan(350);
    expect(page.getByRole('dialog').length).toBe(2);
  });
  it('keeps nested keyboard scrolling in the child instead of activating its parent', async () => {
    await page.viewport(1200, 900);
    render(<div style={{ padding: 80 }}>
      <HoverPreview title="Parent" trigger={(activate) => <button onClick={activate}>Parent</button>}>
        <HoverPreview title="Child" trigger={(activate) => <button onClick={activate}>Child</button>}>
          <div>{Array.from({ length: 70 }, (_, i) => <p key={i}>Child reading {i}</p>)}</div>
        </HoverPreview>
      </HoverPreview>
    </div>);
    await userEvent.tab();
    await userEvent.keyboard('{ArrowDown}');
    await userEvent.tab();
    expect(document.activeElement).toBe(page.getByRole('button', { name: 'Child', exact: true }).element());
    await userEvent.keyboard('{ArrowDown}');
    const childContent = page.getByRole('region', { name: 'Preview content: Child' }).element() as HTMLElement;
    await expect.poll(() => document.activeElement).toBe(childContent);
    await userEvent.keyboard('{ArrowDown}');
    expect(document.activeElement).toBe(childContent);
    await expect.poll(() => childContent.scrollTop).toBeGreaterThan(0);
    const before = performance.now();
    await expect.poll(() => performance.now() - before).toBeGreaterThan(350);
    expect(page.getByRole('dialog').length).toBe(2);
  });
  it('clamps a ready preview after viewport resize and closes with Escape', async () => {
    await page.viewport(1200, 900); mount();
    await page.getByRole('button', { name: 'Long notes', exact: true }).click();
    const card = document.querySelector<HTMLElement>('[data-nc-link-preview]')!;
    await page.viewport(390, 650);
    await expect.poll(() => card.getBoundingClientRect().right).toBeLessThanOrEqual(379);
    expect(card.getBoundingClientRect().left).toBeGreaterThanOrEqual(11);
    expect(card.getBoundingClientRect().bottom).toBeLessThanOrEqual(639);
    await userEvent.keyboard('{Escape}');
    await expect.poll(() => document.querySelector('[data-nc-link-preview]')).toBeNull();
  });
});
