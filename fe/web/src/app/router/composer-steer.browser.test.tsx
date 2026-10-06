import { render } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';

import '../../styles/entry.css';
import { ChatComposer } from '../../features/chat/thread/public.tsx';
import { PendingQueue } from '../../features/planner/pending-queue.tsx';

afterEach(() => { document.body.replaceChildren(); });

describe('follow-up controls in Chromium', () => {
  it('steers directly by keyboard and keeps controls inside a narrow composer', async () => {
    const onSend = vi.fn(); const onSteer = vi.fn();
    render(<div style={{ width: 290 }}>
      <ChatComposer onSend={onSend} onSteer={onSteer} onStop={vi.fn()}
        drawer={<PendingQueue entries={[{ entry_id: 'e1', text: 'Check the tests before continuing', rev: 0, queued_at_ms: 1 }]}
          overflow={0} busy={false} onEdit={vi.fn(() => Promise.resolve({ kind: 'done' as const }))}
          onDelete={vi.fn(() => Promise.resolve({ kind: 'done' as const }))}
          onSteer={vi.fn(() => Promise.resolve({ kind: 'done' as const }))} />} />
    </div>);
    const field = page.getByRole('textbox', { name: 'Message' });
    await field.fill('Keep the fix focused');
    await userEvent.keyboard('{Control>}{Shift>}{Enter}{/Shift}{/Control}');
    expect(onSteer).toHaveBeenCalledWith('Keep the fix focused');
    expect(onSend).not.toHaveBeenCalled();
    await field.fill('Add a regression test');
    const composer = document.querySelector<HTMLElement>('[data-nc-composer]')!;
    expect(composer.scrollWidth).toBeLessThanOrEqual(composer.clientWidth + 1);
    expect(composer.querySelectorAll('button[aria-label="Say it now"]')).toHaveLength(1);
    await expect.element(page.getByRole('button', { name: 'Delete this message' })).toBeVisible();
    await expect.element(page.getByRole('button', { name: 'Queue message' })).toBeVisible();
    const queueText = document.querySelector<HTMLElement>('[data-nc-pending-entry-text]')!;
    const fieldElement = document.querySelector<HTMLElement>('[data-nc-composer] [contenteditable="true"]')!;
    const textBounds = (element: Element): DOMRect => {
      const range = new Range(); range.selectNodeContents(element); return range.getBoundingClientRect();
    };
    expect(Math.abs(textBounds(queueText).left - textBounds(fieldElement).left)).toBeLessThanOrEqual(1);
    const bubble = document.querySelector<HTMLElement>('[data-nc-pending-bubble]')!;
    const queueDelete = composer.querySelector<HTMLButtonElement>('button[aria-label="Delete this message"]')!;
    const stop = composer.querySelector<HTMLButtonElement>('button[aria-label="Stop"]')!;
    expect(Math.abs(queueDelete.getBoundingClientRect().right - stop.getBoundingClientRect().right)).toBeLessThanOrEqual(1);
    expect(bubble.getBoundingClientRect().height).toBeLessThanOrEqual(32);
    const well = fieldElement.parentElement!.parentElement!.parentElement!;
    expect(well.getBoundingClientRect().top - bubble.getBoundingClientRect().bottom).toBeGreaterThanOrEqual(12);
    await page.getByRole('group', { name: 'Message composer' }).screenshot({ path: '__screenshots__/composer-steer-preview.png' });
  });
});
