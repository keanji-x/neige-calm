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
    await page.getByRole('group', { name: 'Message composer' }).screenshot({ path: '__screenshots__/composer-steer-preview.png' });
  });
});
