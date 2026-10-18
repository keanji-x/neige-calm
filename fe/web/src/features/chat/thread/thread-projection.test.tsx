import { act, cleanup, fireEvent, render, renderHook, screen } from '@testing-library/react';
import { startTransition, Suspense, use } from 'react';
import { flushSync } from 'react-dom';
import { afterEach, expect, it, vi } from 'vitest';
import { SYSTEM_PRESENTATION_LABELS, type Conversation, type ConversationActivity, type ConversationSystemEntry,
  type ConversationTurn, type ConversationTurnOutcome, type TranscriptEntry } from '../../../../../core/domain/conversation.ts';
import { useState } from '../../../ui/state/public.ts';
import { ChatThread } from './public.tsx';
import { useThreadReadingView } from './reading-view.tsx';

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

const conversation: Conversation = {
  provider: 'codex', id: 'projection', trackId: 'track', title: 'Historical tools', kind: 'codex', state: 'idle', updatedAt: 1,
};
function activity(id: string): ConversationActivity {
  return { id, author: 'activity', verb: 'Ran', target: `tool-${id}`, tool: null,
    state: 'done', durationMs: null, detail: null, atMs: 1 };
}
const reply: ConversationTurn = { id: 'reply', author: 'agent', text: 'Current reply', atMs: 2 };

it('does not convert unchanged historical tool bodies when the tail reply grows', () => {
  const tools = Array.from({ length: 24 }, (_, index) => activity(String(index)));
  let conversions = 0;
  for (const tool of tools) {
    const target = tool.target;
    Object.defineProperty(tool, 'target', { enumerable: true, get: () => { conversions++; return target; } });
  }
  const content = (last: ConversationTurn) => <ChatThread conversation={conversation} turns={[...tools, last]}
    hasPendingMessages={false} cards={{}} stalled={false} canContinue={false} />;
  const view = render(content(reply));
  const group = screen.getByRole('group', { name: '24 tool calls' });
  const queries = vi.spyOn(group, 'querySelector');
  expect(conversions).toBeGreaterThan(0);
  conversions = 0;

  view.rerender(content({ ...reply, text: 'Current reply grows' }));

  expect(screen.getByText('Current reply grows')).toBeTruthy();
  expect(screen.getByRole('group', { name: '24 tool calls' })).toBe(group);
  expect(conversions).toBe(0);
  expect(queries).not.toHaveBeenCalled();
});

it('updates changed activity data without replacing the carried group host', () => {
  const a = activity('a');
  const b = activity('b');
  const content = (calls: readonly ConversationActivity[]) => <ChatThread conversation={conversation} turns={[...calls, reply]}
    hasPendingMessages={false} cards={{}} stalled={false} canContinue={false} />;
  const view = render(content([a, b]));
  const group = screen.getByRole('group', { name: '2 tool calls' });
  act(() => { fireEvent.click(group.querySelector<HTMLElement>('[aria-expanded]')!); });
  view.rerender(content([a, { ...b, target: 'changed target', state: 'failed', detail: 'changed failure evidence' }]));
  expect(screen.getByRole('group', { name: '2 tool calls' })).toBe(group);
  act(() => { fireEvent.click(screen.getByRole('button', { name: /changed target/ })); });
  expect(screen.getByText('changed failure evidence')).toBeTruthy();
});

it('keeps quiet activities inline and mounted while reusing their unchanged inputs', () => {
  const opening: ConversationSystemEntry = { id: 'report', author: 'system', quiet: true,
    label: SYSTEM_PRESENTATION_LABELS.system_report_edited, text: 'Report edited (author = "user").', atMs: 1 };
  const complete: ConversationTurnOutcome = { id: 'complete', author: 'turn', turnId: 'turn', elapsedMs: null, status: 'completed', atMs: 2 };
  const calls = [activity('a'), activity('b')];
  let conversions = 0;
  for (const call of calls) {
    const target = call.target;
    Object.defineProperty(call, 'target', { enumerable: true, get: () => { conversions++; return target; } });
  }
  const boundary: ConversationTurn = { id: 'boundary', author: 'you', text: 'Next question', atMs: 3 };
  const content = (tail: ConversationTurn, lifted: readonly TranscriptEntry[] = []) => <ChatThread conversation={conversation}
    turns={[opening, ...calls, complete, ...lifted, boundary, tail]} hasPendingMessages={false} cards={{}} stalled={false} canContinue={false} />;
  const view = render(content(reply));
  const fold = document.querySelector<HTMLElement>('[data-nc-turn="quiet-sync"]')!;
  const children = [...fold.querySelectorAll('[data-nc-entry]')];
  expect(fold.hasAttribute('open')).toBe(false);
  expect(fold.getAttribute('data-nc-quiet-sync-outcome')).toBe('accepted');
  expect(fold.querySelector('[role="group"]')).toBeNull();
  expect(fold.textContent).toContain('tool-a');
  expect(fold.textContent).toContain('tool-b');
  conversions = 0;
  view.rerender(content({ ...reply, text: 'Current reply grows' }));
  expect([...fold.querySelectorAll('[data-nc-entry]')]).toEqual(children);
  expect(conversions).toBe(0);
  view.rerender(content(reply, [{ id: 'ask', author: 'agent', origin: 'ask', text: 'Lifted question', atMs: 2 }]));
  expect(fold.hasAttribute('data-nc-quiet-sync-outcome')).toBe(false);
  expect(screen.getByText('Lifted question')).toBeTruthy();
  expect(conversions).toBe(0);
});

it('recomputes exchange positions for retained message bodies after a prepend', () => {
  const question: ConversationTurn = { id: 'question', author: 'you', text: 'Question', atMs: 1 };
  const content = (entries: readonly TranscriptEntry[]) => <ChatThread conversation={conversation} turns={entries}
    hasPendingMessages={false} cards={{}} stalled={false} canContinue={false} />;
  const view = render(content([question, reply]));
  const host = document.querySelector('[data-nc-entry="question"]')!;
  expect(host.getAttribute('data-nc-exchange')).toBe('question');
  view.rerender(content([{ id: 'earlier', author: 'you', text: 'Earlier question', atMs: 0 }, question, reply]));
  expect(document.querySelector('[data-nc-entry="question"]')).toBe(host);
  expect(host.hasAttribute('data-nc-exchange')).toBe(false);
  expect(document.querySelector('[data-nc-exchange="earlier"]')).toBeTruthy();
});

it('keeps the committed body cache when another transcript render is abandoned', async () => {
  const calls = [activity('a'), activity('b')];
  let conversions = 0;
  for (const call of calls) {
    const target = call.target;
    Object.defineProperty(call, 'target', { enumerable: true, get: () => { conversions++; return target; } });
  }
  let changeMode: (mode: 'shown' | 'speculative' | 'returned') => void = () => { throw new Error('Harness not mounted'); };
  let didSuspend = false;
  const never = new Promise<never>(() => {});
  function Suspend() { didSuspend = true; return use(never); }
  function Harness() {
    const [mode, setMode] = useState<'shown' | 'speculative' | 'returned'>('shown');
    changeMode = setMode;
    return <Suspense fallback={<p>Waiting</p>}>
      <ChatThread conversation={conversation} turns={mode === 'speculative' ? [activity('stranger'), reply] : [...calls, reply]}
        hasPendingMessages={false} cards={{}} stalled={false} canContinue={false} />
      {mode === 'speculative' && <Suspend />}
    </Suspense>;
  }
  render(<Harness />);
  const group = screen.getByRole('group', { name: '2 tool calls' });
  await act(async () => { startTransition(() => { changeMode('speculative'); }); await Promise.resolve(); });
  expect(didSuspend).toBe(true);
  expect(screen.getByRole('group', { name: '2 tool calls' })).toBe(group);
  conversions = 0;
  act(() => { flushSync(() => { changeMode('returned'); }); });
  expect(screen.getByRole('group', { name: '2 tool calls' })).toBe(group);
  expect(conversions).toBe(0);
});

it('keeps reading commands stable, suppresses no-ops and rejects a replaced owner’s callbacks', () => {
  const hook = renderHook(({ id }) => useThreadReadingView(id), { initialProps: { id: 'first' } });
  const original = hook.result.current;
  act(() => { original.updateGroup('run', ui => ui); original.setExpanded('quiet', false); });
  expect(hook.result.current).toBe(original);
  act(() => { original.setExpanded('quiet', true); });
  expect(hook.result.current.setExpanded).toBe(original.setExpanded);
  expect(hook.result.current.updateGroup).toBe(original.updateGroup);
  hook.rerender({ id: 'second' });
  hook.rerender({ id: 'first' });
  const replacement = hook.result.current;
  expect(replacement.groupKeys).not.toBe(original.groupKeys);
  act(() => { original.setExpanded('quiet', true); original.updateGroup('run', ui => ({ ...ui, expanded: true })); });
  expect(hook.result.current).toBe(replacement);
  expect(hook.result.current.groups.size).toBe(0);
  expect(hook.result.current.expanded.size).toBe(0);
});
