import { createMemoryHistory, RouterProvider } from '@tanstack/react-router';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { commands, page, userEvent } from 'vitest/browser';
import { Dialog } from '../../ui/dialog/public.tsx';
import { DepthFeedback } from '../../ui/motion/depth-feedback.tsx';
import type { ApiRequest, ApiTransportPort } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { prepareReportDocument } from '../../features/report/document/preparation.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';
import '../../styles/entry.css';

afterEach(cleanup);

function mount() {
  const creates: ApiRequest[] = [];
  const transport: ApiTransportPort = { send(request) {
    if (request.method === 'POST' && request.path === '/api/tracks') {
      creates.push(request);
      return Promise.resolve({ status: 400, statusText: 'Bad Request', body: {
        code: 'bad_request', error: 'Folder overlaps Neige-owned location /repo/.claude/worktrees/track-other',
      } });
    }
    const body = request.path === '/api/areas' ? [{ id: 'a', name: 'Work', color: '#5B8DEF',
      sort: 0, kind: 'user', default_cwd: '/repo', created_at: 1, updated_at: 1 }]
      : request.path.startsWith('/api/fs/listdir') ? { path: new URL(request.path, 'http://test').searchParams.get('path') ?? '/repo', parent: '/',
        entries: Array.from({ length: 16 }, (_, i) => ({ name: `folder-${i.toString().padStart(2, '0')}`, is_dir: true })) }
      : request.path === '/api/settings' ? {} : [];
    return Promise.resolve({ status: 200, statusText: 'OK', body });
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ prepareReport: prepareReportDocument, transport, client, cards: bootTestCardRuntime(),
    unauthorized: createUnauthorizedChannel({ enqueue: (task) => task() }), onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: ['/area/a/new'] }) });
  render(<><DepthFeedback /><QueryClientProvider client={client}><ThemeProvider><RouterProvider router={router} /></ThemeProvider></QueryClientProvider></>);
  return { creates, router };
}

it.each([1280, 390])('composer isolation option, badge, explanation and admission refusal (%ipx)', async (width) => {
  await page.viewport(width, 900);
  const { creates } = mount();
  const trigger = page.getByRole('button', { name: 'Workspace isolation: Worktree' });
  await expect.element(trigger).toBeVisible();
  expect(trigger.element().closest('[data-nc-composer-preferences]')).toBeTruthy();
  await trigger.click();
  await expect.element(page.getByRole('menuitem', { name: /Worktree.*Selected/ })).toBeVisible();
  await page.getByRole('menuitem', { name: /Worktree/ }).click();
  await page.getByRole('combobox', { name: 'What this track should do' }).fill('Plan');
  await page.getByRole('button', { name: 'Create track' }).click();
  await expect.poll(() => creates.length).toBe(1);
  expect(creates[0]?.body).not.toHaveProperty('isolation');

  await trigger.click();
  await page.getByRole('menuitem', { name: 'None' }).click();
  const badge = page.getByRole('button', { name: 'Not isolated · no guarantees' });
  await expect.element(badge).toBeVisible();
  await badge.click();
  await expect.element(page.getByText(/Planner commits land on its current branch/)).toBeVisible();
  await expect.element(page.getByText(/task workers are unavailable/i)).toBeVisible();
  await expect.element(page.getByText(/#2862/)).toBeVisible();
  await badge.click();
  await page.getByRole('button', { name: 'Create track' }).click();
  await expect.poll(() => creates.length).toBe(2);
  expect(creates[1]?.body).toMatchObject({ cwd: '/repo', isolation: 'none', first_message: 'Plan' });
  await expect.element(page.getByText('Folder overlaps Neige-owned location /repo/.claude/worktrees/track-other')).toBeVisible();
  expect(document.documentElement.scrollWidth).toBe(width);
  if (width === 390) await page.getByRole('button', { name: 'Track options', exact: true }).click();
  await page.getByRole('button', { name: 'Use a Neige workspace instead' }).click();
  if (width === 390) await page.getByRole('button', { name: 'Track options', exact: true }).click();
  const privateTrigger = page.getByRole('button', { name: 'Workspace isolation: None' });
  await expect.element(privateTrigger).toBeVisible();
  await expect.element(badge).not.toBeInTheDocument();
  await privateTrigger.click();
  const disabled = page.getByRole('menuitem', { name: /Worktree/ });
  await expect.element(disabled).toHaveAttribute('aria-disabled', 'true');
  await expect.element(page.getByText('Private Neige folder: no worktree. Choose a Git folder to use one.')).toBeVisible();
  await expect.element(page.getByRole('menuitem', { name: /None.*Selected/ })).toBeVisible();
  expect(document.documentElement.scrollWidth).toBe(width);
});

// Real router, transport decoder, production form/Popover/Dialog and delegated
// press adapter. Directory IO uses the transport fixture; no copied handoff policy.
it.each([
  { width: 390, keyboard: false, reduced: false },
  { width: 390, keyboard: true, reduced: false },
  { width: 390, keyboard: true, reduced: true },
  { width: 1280, keyboard: false, reduced: false },
])('directory handoff owns hits, focus and reopen ($width/$keyboard/$reduced)', async ({ width, keyboard, reduced }) => {
  await commands.emulateReducedMotion(reduced);
  try {
    await page.viewport(width, 844);
    mount();
    const more = page.getByRole('button', { name: 'Track options', exact: true });
    const folder = page.getByRole('button', { name: /^Folder:/ });
    const dialog = page.getByRole('dialog', { name: 'Choose a directory' });
    const field = page.getByRole('combobox', { name: 'Directory path' });
    await page.getByRole('combobox', { name: 'What this track should do' }).fill('Keep my draft');
    for (const select of [false, true]) {
      if (width === 390) {
        if (keyboard) { more.element().focus(); await userEvent.keyboard('{Enter}'); }
        else await more.click();
        await expect.element(page.getByRole('dialog', { name: 'Track options', exact: true })).toHaveAttribute('aria-modal', 'true');
      }
      await expect.element(folder).toBeVisible();
      const source = folder.element().parentElement;
      const moreElement = width === 390 ? more.element() : null;
      if (keyboard) { folder.element().focus(); await userEvent.keyboard('{Enter}'); }
      else await folder.click();
      await expect.element(dialog).toBeVisible();
      // A visible body portal alone proves nothing about native top-layer hits.
      await expect.poll(() => document.querySelectorAll(':popover-open').length).toBe(0);
      if (width === 390) {
        expect(moreElement).toHaveAttribute('aria-expanded', 'false');
        expect(source?.getClientRects()).toHaveLength(0); // revoked source: no rendered box, no cached substitute
      }
      await expect.element(field).toHaveValue('/repo/');
      await expect.element(field).toHaveFocus();
      const list = page.getByRole('listbox', { name: 'Directory entries' }).element();
      const bounds = list.getBoundingClientRect();
      const visible = [...list.querySelectorAll<HTMLElement>('[role="option"]')].filter(e => {
        const r = e.getBoundingClientRect(); return r.top >= bounds.top && r.bottom <= bounds.bottom;
      });
      expect(visible.length).toBeGreaterThan(5);
      for (const row of visible) {
        const r = row.getBoundingClientRect();
        expect(row.contains(document.elementFromPoint(r.left + 40, r.top + r.height / 2))).toBe(true);
      }
      // Include the lower list that the old options card actually covered.
      await userEvent.click(visible.at(-1)!);
      await expect.element(field).toHaveValue(`/repo/${visible.at(-1)!.textContent?.trim()}/`);
      for (let i = 0; i < 22; i++) {
        await userEvent.keyboard('{Tab}');
        expect(dialog.element().contains(document.activeElement)).toBe(true);
      }
      if (select) {
        await page.getByRole('button', { name: 'Select this directory' }).click();
      } else {
        await page.getByRole('button', { name: 'Cancel', exact: true }).click();
      }
      await expect.element(dialog).not.toBeInTheDocument();
      await expect.element(width === 390 ? more : folder).toHaveFocus();
      await expect.element(page.getByRole('combobox', { name: 'What this track should do' })).toHaveTextContent('Keep my draft');
    }
    if (width === 390) await more.click();
    await folder.click();
    await expect.element(field).toHaveFocus();
    // Closing after a layout transition must choose the currently reachable control.
    await page.viewport(width === 390 ? 1280 : 390, 844);
    await userEvent.keyboard('{Escape}');
    await expect.element(dialog).not.toBeInTheDocument();
    await expect.element(width === 390 ? folder : more).toHaveFocus();
  } finally { await commands.emulateReducedMotion(false); }
});

// Control the close→paint gap only. Real pointer/keyboard and the production
// Router/Form/Dialog still run; this measures ownership, not race frequency.
function holdFrames() {
  const pending = new Map<number, FrameRequestCallback>();
  let sequence = 0;
  const cancel = window.cancelAnimationFrame.bind(window);
  const request = vi.spyOn(window, 'requestAnimationFrame').mockImplementation(callback => {
    pending.set(--sequence, callback);
    return sequence;
  });
  const cancellation = vi.spyOn(window, 'cancelAnimationFrame').mockImplementation(id => {
    if (!pending.delete(id)) cancel(id);
  });
  return {
    pending,
    drain() {
      const callbacks = [...pending.values()]; pending.clear();
      for (const callback of callbacks) callback(performance.now());
    },
    restore() { request.mockRestore(); cancellation.mockRestore(); },
  };
}

it.each([1440, 390].flatMap(width => ['Cancel', 'Select this directory'].flatMap(close =>
  ['pointer', 'keyboard', 'focus'].map(intent => ({ width, close, intent })),
)))('directory close yields to new editor owner ($width/$close/$intent)', async ({ width, close, intent }) => {
  await page.viewport(width, 844);
  mount();
  const editor = page.getByRole('combobox', { name: 'What this track should do' });
  const more = page.getByRole('button', { name: 'Track options', exact: true });
  await expect.element(editor).toHaveFocus();
  if (width === 390) await more.click();
  await page.getByRole('button', { name: /^Folder:/ }).click();
  await expect.element(page.getByRole('combobox', { name: 'Directory path' })).toHaveValue('/repo/');
  await expect.element(page.getByRole('combobox', { name: 'Directory path' })).toHaveFocus();
  await new Promise(requestAnimationFrame);
  await new Promise(requestAnimationFrame);
  const frames = holdFrames();
  try {
    await page.getByRole('button', { name: close, exact: true }).click();
    await expect.element(page.getByRole('dialog', { name: 'Choose a directory' })).not.toBeInTheDocument();
    expect(frames.pending.size).toBeGreaterThan(0);
    if (intent === 'pointer') await editor.click();
    else if (intent === 'focus') editor.element().focus();
    else {
      // Navigate from the actual cleanup opener, without a synthetic focus event.
      for (let i = 0; i < 8 && document.activeElement !== editor.element(); i++) {
        await userEvent.keyboard('{Shift>}{Tab}{/Shift}');
      }
    }
    expect(document.activeElement).toBe(editor.element());
    frames.drain();
    expect(document.activeElement).toBe(editor.element());
  } finally { frames.restore(); }
});

it.each([1440, 390].flatMap(width => ['refold', 'reopen', 'route', 'modal', 'unmount'].map(edge => ({ width, edge }))))(
  'directory restoration expires across $edge ($width)', async ({ width, edge }) => {
    await page.viewport(width, 844);
    const { router } = mount();
    const more = page.getByRole('button', { name: 'Track options', exact: true });
    const folder = page.getByRole('button', { name: /^Folder:/ });
    const field = page.getByRole('combobox', { name: 'Directory path' });
    if (width === 390) await more.click();
    await folder.click();
    await expect.element(field).toHaveValue('/repo/');
    await expect.element(field).toHaveFocus();
    await new Promise(requestAnimationFrame);
    await new Promise(requestAnimationFrame);
    const frames = holdFrames();
    try {
      await userEvent.keyboard('{Escape}');
      await expect.element(page.getByRole('dialog', { name: 'Choose a directory' })).not.toBeInTheDocument();
      if (edge === 'refold') {
        await page.viewport(width === 390 ? 1440 : 390, 844);
        frames.drain();
        await expect.element(width === 390 ? folder : more).toHaveFocus();
      } else if (edge === 'reopen') {
        if (width === 390) await more.click();
        await folder.click();
        // Opening focus itself is frame-scheduled; explicitly choose the new
        // modal's input before releasing either presentation's pending work.
        field.element().focus();
        expect(document.activeElement).toBe(field.element());
        frames.drain();
        expect(document.activeElement).toBe(field.element());
      } else {
        if (edge === 'unmount') cleanup();
        else if (edge === 'route') await router.navigate({ to: '/' });
        else {
          render(<Dialog origin={{ kind: 'surface-center' }} open title="New modal" onClose={() => undefined}>
            <button type="button">New modal owner</button>
          </Dialog>);
          page.getByRole('button', { name: 'New modal owner' }).element().focus();
        }
        const owner = document.activeElement;
        frames.drain();
        expect(document.activeElement).toBe(owner);
        if (edge === 'modal') expect(page.getByRole('dialog', { name: 'New modal' }).element().contains(document.activeElement)).toBe(true);
      }
    } finally { frames.restore(); }
  },
);
