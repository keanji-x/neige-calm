/* Exercise the real Edit action and composer: mode changes are local and text never scales. */
import { act, cleanup, render, screen } from '@testing-library/react';
import { commands } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';

import '../../../styles/entry.css';
import { ChatComposer, ChatThread } from './public.tsx';
import { useState } from '../../../ui/state/public.ts';
import type { Conversation, TranscriptEntry } from '../../../../../core/domain/conversation.ts';

const TEXT = 'Review the animation system and explain how editing can feel more natural. Keep the message readable and the rest of the conversation still. '.repeat(3);
const conversation: Conversation = Object.freeze({ id: 'edit', trackId: 'track', title: null, kind: 'codex', state: 'idle', updatedAt: 1 });
function Scene({ onUpdate = () => {} }: { onUpdate?: () => void }) {
  const [editing, setEditing] = useState(false);
  const [text, setText] = useState('');
  const turns: TranscriptEntry[] = [
    { id: 'you', author: 'you', text: TEXT, atMs: 1 },
    { id: 'outcome', author: 'turn', turnId: 'turn', status: 'completed', elapsedMs: null, atMs: 2 },
  ];
  return <div data-nc-drawer="" style={{ width: 440 }}>
    <ChatThread conversation={conversation} turns={turns} cards={{}} stalled={false} canContinue={false}
      editing={editing ? 'outcome' : null} editMessage={() => { onUpdate(); setEditing(true); setText(TEXT); }} />
    <ChatComposer onSend={() => {}} draft={{ text, onChange: setText }} focusRequest={editing ? 1 : 0}
      {...(editing ? { editing: { preview: TEXT, onCancel: () => { setEditing(false); setText(''); } } } : {})} />
  </div>;
}
const size = () => document.querySelector<HTMLElement>('[data-nc-size-motion]')!;
const field = () => document.querySelector<HTMLElement>('[contenteditable="true"]')!;
const frame = () => new Promise<void>(resolve => requestAnimationFrame(() => resolve()));
const edit = () => act(() => { screen.getByRole('button', { name: 'Edit message' }).click(); });
const cancel = () => act(() => { screen.getByRole('button', { name: 'Cancel edit' }).click(); });
const settled = () => expect.poll(() => size().style.height).toBe('');

afterEach(async () => { cleanup(); vi.restoreAllMocks(); await commands.emulateReducedMotion(false); });

it('enters edit immediately without a document transition or scaled text', () => {
  const start = vi.spyOn(document, 'startViewTransition');
  const update = vi.fn();
  render(<Scene onUpdate={update} />);
  edit();
  expect(update).toHaveBeenCalledOnce();
  expect(start).not.toHaveBeenCalled();
  expect(field().textContent).toBe(TEXT);
  expect(document.activeElement).toBe(field());
  expect(getComputedStyle(field()).transform).toBe('none');
  expect(document.querySelector('[data-nc-turn="you"]')?.hasAttribute('data-nc-editing')).toBe(true);
});

it('expands and contracts the live composer locally, then releases its height', async () => {
  render(<Scene />);
  await frame();
  const initial = size().getBoundingClientRect().height;
  edit();
  expect(size().style.height).not.toBe('');
  await frame();
  const natural = size().firstElementChild!.getBoundingClientRect().height;
  expect(natural).toBeGreaterThan(initial);
  expect(size().getBoundingClientRect().height).toBeLessThan(natural);
  expect(getComputedStyle(size()).transform).toBe('none');
  await settled();
  expect(size().getBoundingClientRect().height).toBeCloseTo(natural, 0);
  cancel();
  expect(size().style.height).not.toBe('');
  expect(field().textContent).toBe('');
  await settled();
  expect(size().getBoundingClientRect().height).toBeCloseTo(initial, 0);
  expect(size().style.overflow).toBe('');
});

it('reverses from the painted size without losing the caret or retaining stale inline styles', async () => {
  render(<Scene />);
  edit();
  await frame();
  const interrupted = size().getBoundingClientRect().height;
  cancel();
  expect(size().getBoundingClientRect().height).toBeCloseTo(interrupted, 0);
  edit();
  expect(field().textContent).toBe(TEXT);
  expect(document.activeElement).toBe(field());
  // Input remains live during animation.
  act(() => { field().textContent = 'Still editable'; field().dispatchEvent(new InputEvent('input', { bubbles: true })); });
  expect(field().textContent).toBe('Still editable');
  await settled();
  expect(size().style.overflow).toBe('');
});

it('skips travel under reduced motion and stops a transition when the preference changes', async () => {
  await commands.emulateReducedMotion(true);
  render(<Scene />);
  edit();
  expect(field().textContent).toBe(TEXT);
  expect(size().style.height).toBe('');
  cancel();
  await commands.emulateReducedMotion(false);
  edit();
  expect(size().style.height).not.toBe('');
  await commands.emulateReducedMotion(true);
  await expect.poll(() => size().style.height).toBe('');
  expect(field().textContent).toBe(TEXT);
});
