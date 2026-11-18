/** Isolated controlled port, production card registry, renderer and styles; no live AGY process. */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, render, screen, waitFor } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { expect, vi } from 'vitest';
import type { NativeWorkerRead, NativeWorkerPort } from '../../../../../core/domain/native-worker.ts';
import type { TaskAttempt } from '../../../../../core/domain/task-recovery.ts';
import { trackDetail } from '../../../../../core/events/query-keys.ts';
import { CardDockHost, createCardHost, createCardRegistry, registerAvailableBuiltinCards } from '../../../systems/cards/public.ts';

export async function exerciseNativeWorker(width: number) {
  await page.viewport(width, 1000);
  const registry = createCardRegistry(); registerAvailableBuiltinCards(registry);
  const card = registry.resolve({ id: 'fixture-native-worker', kind: 'codex', payload: { acp_worker: true, track_id: 't', task_key: 'inspect', idempotency_key: 'a' },
    runtime: { worker_session_id: 'worker', kind: 'codex', status: 'running', provider: 'antigravity' } });
  if (card === null) throw new Error('Native Worker did not resolve');
  expect(card.type).toBe('native-worker');
  const attempt: TaskAttempt = { attempt_id: 'a', generation: 1, status: 'running', status_detail: null, blocking_reason: null,
    worker_card_id: card.id, created_at_ms: 0, finished_at_ms: null, worker_selection: { mechanism: 'agent', provider: 'antigravity',
      requested_model: null, requested_effort: 'deep', effective_model: 'fixture/provider-default', effective_effort: 'deep',
      selection_source: { model: 'provider_default_reported', effort: 'explicit' }, turn_phase: 'submitting', cleanup_pending: false,
      capabilities: { provider: 'antigravity', operation: 'acp-worker', message_transport: 'native_session', cancel_strategy: 'acp_cancel', resumable: true, executor: true, validator: true } } };
  let snapshot: NativeWorkerRead = { flow: '# Controlled ACP flow\n\nCaptured Worker response\n\n' + 'long-output-'.repeat(80), attempt, currentAttemptId: 'a' };
  const read = vi.fn<NativeWorkerPort['read']>(() => Promise.resolve(snapshot));
  const message = vi.fn<NativeWorkerPort['message']>(() => Promise.resolve());
  const cancel = vi.fn<NativeWorkerPort['cancel']>(() => { snapshot = { ...snapshot, attempt: { ...attempt, status: 'canceled', worker_selection: { ...attempt.worker_selection!, turn_phase: 'unknown', cleanup_pending: true } } }; return Promise.resolve(); });
  const host = createCardHost(registry, { nativeWorker: { read, message, cancel } });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const item = { card, title: 'Antigravity Worker', originalIndex: 0, activity: null, notice: null };
  render(<QueryClientProvider client={client}><div style={{ inlineSize: '100%', maxInlineSize: 900, blockSize: 900 }}>
    <CardDockHost host={host} item={item} visible focused />
  </div></QueryClientProvider>);
  await waitFor(() => expect(read).toHaveBeenCalled());
  await expect.element(page.getByText('Requested model: Provider default · effort: deep', { exact: true })).toBeVisible();
  await expect.element(page.getByText(/fixture\/provider-default/)).toBeVisible();
  expect(document.querySelector('[data-nc-terminal-card]')).toBeNull();
  const textarea = screen.getByRole('textbox', { name: 'User message' });
  await userEvent.fill(textarea, 'Please inspect the recorded evidence');
  await userEvent.click(screen.getByRole('button', { name: 'Queue message' }));
  await waitFor(() => expect(message).toHaveBeenCalledTimes(1));
  expect(message.mock.calls[0]).toEqual(['t', 'inspect', 'a', expect.any(String), 'Please inspect the recorded evidence']);
  await expect.element(page.getByText('User message queued · the Worker must read it before reporting')).toBeVisible();
  const initialBox = textarea.getBoundingClientRect();
  expect(initialBox.width).toBeGreaterThan(100); expect(initialBox.right).toBeLessThanOrEqual(width);
  expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(width);
  await page.screenshot({ path: `test-results/gemini-native-worker-${width}-long-fixture.png` });
  snapshot = { ...snapshot, flow: '# Updated native activity\n\nFollow-up captured on the same native session.' };
  await act(() => client.invalidateQueries({ queryKey: trackDetail('t') }));
  await waitFor(() => expect(document.querySelector('[aria-label="Worker flow"]')?.textContent).toContain('Updated native activity'));
  const box = textarea.getBoundingClientRect();
  expect(box.width).toBeGreaterThan(100); expect(box.right).toBeLessThanOrEqual(width);
  expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(width);
  await page.screenshot({ path: `test-results/gemini-native-worker-${width}-fixture.png` });
  await userEvent.click(screen.getByRole('button', { name: 'Cancel task' }));
  await waitFor(() => expect(cancel).toHaveBeenCalledTimes(1));
  await expect.element(page.getByText('Unknown · cleanup pending; workspace remains occupied')).toBeVisible();
  expect(screen.getByRole('button', { name: 'Queue message' })).toBeDisabled();
  await page.screenshot({ path: `test-results/gemini-native-worker-${width}-unknown-fixture.png` });
  // A retained old card keeps its own request/actual selection after a new attempt appears.
  snapshot = { ...snapshot, currentAttemptId: 'successor', attempt: { ...attempt, status: 'failed' } };
  await act(() => client.invalidateQueries({ queryKey: trackDetail('t') }));
  await waitFor(() => expect(screen.getByRole('button', { name: 'Queue message' })).toBeDisabled());
  await expect.element(page.getByText('Requested model: Provider default · effort: deep', { exact: true })).toBeVisible();
  expect(read.mock.calls.every(args => args.length === 5)).toBe(true);
  client.clear();
}

/** Isolated authored Task rows use the same inventory and provider presentation as live reads. */
export async function exerciseProviderRows(width: number) {
  await page.viewport(width, 900);
  const { TrackPage } = await import('./public.tsx');
  const { NEUTRAL_ACTIVITY } = await import('../../../../../core/domain/track.ts');
  const rows = [
    { blockId: 'gemini', key: 'gemini-review', state: 'ready' as const, declaration: null, status: 'running' as const,
      statusDetail: null, kind: 'agent' as const, provider: 'antigravity' as const, workerCardId: null, pendingReason: null },
    { blockId: 'legacy', key: 'legacy-codex', state: 'ready' as const, declaration: null, status: 'done' as const,
      statusDetail: null, kind: 'codex' as const, workerCardId: null, pendingReason: null },
    { blockId: 'claude', key: 'claude-summary', state: 'ready' as const, declaration: null, status: 'running' as const,
      statusDetail: null, kind: 'agent' as const, provider: 'claude' as const, workerCardId: null, pendingReason: null },
  ];
  render(<div style={{ inlineSize: width }}><TrackPage mobilePanelObscured={false} panel={width === 420 ? 'tasks' : undefined} cards={[]} tasks={rows} openableCards={new Set()} existingCardIds={new Set()}
    track={{ id: 't', title: 'Controlled provider tasks', isolation: 'worktree',
      closedAt: null, ...NEUTRAL_ACTIVITY }}
    onOpenTask={() => undefined} canReopenTrack={false} canCloseTrack={false} adminGrantedAt={null} onRenameTrack={() => undefined}
    closedControl={{ pending: false, setClosed: () => Promise.resolve(true), feedback: { error: null, clear: () => undefined, run: () => Promise.resolve(true) } }}
    onDeleteTrack={() => undefined} onTrackDeleted={() => undefined} /></div>);
  const scope = width === 420 ? document.querySelector('[data-nc-mobile-panel]')! : document.querySelector('[data-nc-work-inventory]')!;
  for (const summary of scope.querySelectorAll<HTMLElement>('button[aria-expanded="false"][aria-controls]:not([aria-haspopup])')) await userEvent.click(summary);
  for (const summary of document.querySelectorAll<HTMLElement>('[data-nc-mobile-panel] details:not([open]) > summary')) await userEvent.click(summary);
  for (const kind of ['antigravity', 'codex', 'claude']) expect(document.body.textContent).toContain(kind);
  const providerRow = (width === 420 ? scope.querySelector('[data-nc-row="gemini"]') : scope.querySelector('[data-nc-row="task:gemini"]'))!;
  expect(providerRow.querySelector('[data-nc-field="kind"]')?.textContent).toBe('antigravity');
  const providerIcon = width === 420
    ? providerRow.querySelector<SVGElement>('[data-nc-field="kind"] > svg')
    : providerRow.querySelector<SVGElement>('button[data-nc-row-action="reveal-block"] > svg');
  expect(providerIcon).not.toBeNull();
  expect(providerIcon!.classList.contains('lucide-bot')).toBe(true);
  const providerIconBox = providerIcon!.getBoundingClientRect();
  expect(providerIconBox.width).toBeGreaterThan(0);
  expect(providerIconBox.height).toBeGreaterThan(0);
  expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(width);
  await page.screenshot({ path: `test-results/gemini-task-rows-${width}-fixture.png` });
}
