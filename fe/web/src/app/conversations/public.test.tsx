// @vitest-environment jsdom
// The conversation registry's composer refill and outbox bookkeeping, driven through the provider itself.

import { act, cleanup, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';

import { useEffect } from 'react';

import type { PlannerAttachment } from '../../../../core/api/generated/wire.ts';
import { MAX_ATTACHMENTS_PER_MESSAGE, TOO_MANY_IMAGES } from '../../../../core/domain/conversation.ts';
import type { SendOp } from '../../../../core/domain/conversation-outbox.ts';
import type { Conversation } from '../../../../core/domain/conversation.ts';
import { sha256Hex } from '../../../../core/domain/sha256.ts';
import { ConversationProvider, useConversationRegistry, type ConversationRegistry } from './public.tsx';

const CONVERSATION = 'conversation-a';

const image = (id: string): PlannerAttachment => ({ id, contentType: 'image/png', size: 4, url: `/images/${id}` });
const images = (prefix: string, count: number) => Array.from({ length: count }, (_, index) => image(`${prefix}-${index}.png`));

function mountRegistry(): () => ConversationRegistry {
  let registry!: ConversationRegistry;
  function Probe() {
    const current = useConversationRegistry();
    useEffect(() => { registry = current; });
    return null;
  }
  render(<ConversationProvider><Probe /></ConversationProvider>);
  return () => registry;
}

afterEach(cleanup);

/* #2068 item 30: an Edit merges the turn into whatever the composer holds, so the images it could not take are the
   merge's, not the turn's alone; they are said, never dropped silently. */
it('[#2068] says an Edit’s images did not all fit beside the images the composer already holds', () => {
  const registry = mountRegistry();
  const held = images('draft', 3);
  act(() => { registry().editComposer(CONVERSATION, () => ({ text: '', attachments: held })); });
  const turn = images('turn', MAX_ATTACHMENTS_PER_MESSAGE);
  act(() => { registry().beginEdit(CONVERSATION, { kind: 'turn', turnId: 'turn', outcomeId: 'outcome', refill: { text: 'Prompt', attachments: turn } }); });
  expect(registry().composerOf(CONVERSATION).attachments)
    .toEqual([...held, ...turn.slice(0, MAX_ATTACHMENTS_PER_MESSAGE - held.length)]);
  expect(registry().uploadOf(CONVERSATION).refusal).toBe(TOO_MANY_IMAGES);
});

it('says nothing when an Edit’s images all fit beside the composer’s own', () => {
  const registry = mountRegistry();
  act(() => { registry().editComposer(CONVERSATION, () => ({ text: '', attachments: images('draft', 3) })); });
  act(() => { registry().beginEdit(CONVERSATION, { kind: 'turn', turnId: 'turn', outcomeId: 'outcome',
    refill: { text: 'Prompt', attachments: images('turn', MAX_ATTACHMENTS_PER_MESSAGE - 3) } }); });
  expect(registry().composerOf(CONVERSATION).attachments).toHaveLength(MAX_ATTACHMENTS_PER_MESSAGE);
  expect(registry().uploadOf(CONVERSATION).refusal).toBeNull();
});

const op = (key: string, phase: 'confirmed' | 'failed'): SendOp => {
  const echo = { id: `echo-${key}`, author: 'you', text: key, atMs: 1, attachments: [], serverHighWaterBefore: 0, queued: false,
    entryId: null } as const;
  return phase === 'confirmed'
    ? { key, echo, fromComposer: true, replaces: null, phase, queueRevision: null }
    : { key, echo, fromComposer: true, replaces: null, phase, delivery: 'rejected', message: 'Not this time' };
};

/* #2068 item 32: a row stands for at most one send while any send is left; once the outbox is empty, by whatever way
   its last send left, nothing could take that row for a second send and the record goes with it. */
it('[#2068] forgets the rows that retired a send once the outbox empties by a Dismiss', () => {
  const registry = mountRegistry();
  act(() => { registry().editOutbox(CONVERSATION, () => [op('retired', 'confirmed'), op('failed', 'failed')]); });
  act(() => { registry().retireSends(CONVERSATION, [{ key: 'retired', row: '7:row' }]); });
  expect(registry().spentRowsOf(CONVERSATION)).toEqual(['7:row']);
  act(() => { registry().editOutbox(CONVERSATION, (current) => current.filter((held) => held.key !== 'failed')); });
  expect(registry().outboxOf(CONVERSATION)).toEqual([]);
  expect(registry().spentRowsOf(CONVERSATION)).toEqual([]);
});

const queuedInput = Object.freeze({ entry_id: 'middle', text: 'Middle queued input', rev: 2, queued_at_ms: 10 });

it('reuses composer refill and cancel for queued edits, preserving words changed by the reader', () => {
  const registry = mountRegistry();
  const edit = { kind: 'queued', entry: queuedInput, refill: { text: queuedInput.text, attachments: [] } } as const;
  act(() => { registry().beginEdit(CONVERSATION, edit); });
  expect(registry().composerOf(CONVERSATION).text).toBe(queuedInput.text);
  act(() => { registry().cancelEdit(CONVERSATION); });
  expect(registry().composerOf(CONVERSATION).text).toBe('');
  act(() => { registry().beginEdit(CONVERSATION, edit); });
  act(() => { registry().editComposer(CONVERSATION, () => ({ text: 'Keep these unsaved words', attachments: [] })); });
  act(() => { registry().cancelEdit(CONVERSATION); });
  expect(registry().editOf(CONVERSATION)).toBeNull();
  expect(registry().composerOf(CONVERSATION).text).toBe('Keep these unsaved words');
});

it('keeps a queued draft through stale receipts and fences an older edit completion', () => {
  const registry = mountRegistry();
  act(() => { registry().beginEdit(CONVERSATION, { kind: 'queued', entry: queuedInput,
    refill: { text: queuedInput.text, attachments: [] } }); });
  const original = registry().editOf(CONVERSATION);
  if (original?.kind !== 'queued') throw new Error('Expected the queued edit');
  const submitted = { text: 'My revised middle input', attachments: [] };
  act(() => { registry().editComposer(CONVERSATION, () => submitted); });
  act(() => { registry().settleQueuedEdit(CONVERSATION, original, submitted, { kind: 'stale', text: 'Changed elsewhere', rev: 9 }); });
  const current = registry().editOf(CONVERSATION);
  expect(current).toMatchObject({ kind: 'queued', entry: { rev: 9 } });
  expect(registry().composerOf(CONVERSATION)).toBe(submitted);
  act(() => { registry().settleQueuedEdit(CONVERSATION, original, submitted, { kind: 'done', entry: { ...queuedInput, text: submitted.text, rev: 3 } }); });
  expect(registry().editOf(CONVERSATION)).toBe(current);
  expect(registry().composerOf(CONVERSATION)).toBe(submitted);
});

it('settles a queued edit only in its originating conversation and preserves any newer draft', () => {
  const registry = mountRegistry();
  act(() => { registry().beginEdit(CONVERSATION, { kind: 'queued', entry: queuedInput,
    refill: { text: queuedInput.text, attachments: [] } }); });
  const edit = registry().editOf(CONVERSATION);
  if (edit?.kind !== 'queued') throw new Error('Expected the queued edit');
  const submitted = { text: 'Saved text', attachments: [] };
  act(() => {
    registry().editComposer(CONVERSATION, () => ({ text: 'A newer draft', attachments: [] }));
    registry().editComposer('another-conversation', () => ({ text: 'Other conversation draft', attachments: [] }));
  });
  act(() => { registry().settleQueuedEdit(CONVERSATION, edit, submitted, { kind: 'done', entry: { ...queuedInput, text: submitted.text, rev: 3 } }); });
  expect(registry().editOf(CONVERSATION)).toBeNull();
  expect(registry().composerOf(CONVERSATION).text).toBe('A newer draft');
  expect(registry().composerOf('another-conversation').text).toBe('Other conversation draft');
});

it('keeps accepted first-input display separate from stored canonical history and never reimports a retired receipt', () => {
  const registry = mountRegistry();
  const submitted = Object.freeze({ text: 'first words', atMs: 7 });
  const from = { scopeId: 'track', key: 'create-key' };
  const conversation: Conversation = { id: CONVERSATION, trackId: 'track', kind: 'track-assistant', provider: 'codex', title: null,
    state: 'idle', updatedAt: 100 };
  const result = { conversation, initialInput: { kind: 'known', receipt: { version: 'initial-input-v1', entryId: 'first-entry',
    queuedAtMs: 100, textSha256: sha256Hex(submitted.text), initialRuntimeId: 'runtime' } } } as const;
  act(() => {
    registry().remember(conversation, []);
    registry().startDraft({ ...from, provider: 'codex', model: { model: null, reasoning_effort: null }, text: submitted.text,
      submitted, creating: true, error: null, remedy: null });
    expect(registry().acceptDraft(from, result, submitted)).toBe(true);
  });
  expect(registry().draftOf(from.scopeId)).toBeNull();
  expect(registry().turnsOf(CONVERSATION)).toMatchObject([{ text: submitted.text, atMs: submitted.atMs }]);
  expect(registry().rememberedTurnsOf(CONVERSATION)).toEqual([]);
  expect(registry().outboxOf(CONVERSATION)[0]).toMatchObject({ phase: 'initial-accepted', submitted, receipt: result.initialInput.receipt });
  act(() => { registry().retireSends(CONVERSATION, [{ key: from.key, row: null }]); });
  expect(registry().turnsOf(CONVERSATION)).toEqual([]);
  act(() => { expect(registry().acceptDraft(from, result, submitted)).toBe(false); });
  expect(registry().outboxOf(CONVERSATION)).toEqual([]);
});

it('ignores a late acceptance after a valid rekey without touching a newer draft', () => {
  const registry = mountRegistry();
  const submitted = Object.freeze({ text: 'old words', atMs: 3 });
  const from = { scopeId: 'track', key: 'old-key' };
  act(() => { registry().startDraft({ ...from, provider: 'codex', model: { model: null, reasoning_effort: null }, text: 'new words',
    submitted, creating: false, error: null, remedy: null }); });
  act(() => { registry().editDraft(from, current => ({ ...current, key: 'new-key', submitted: null })); });
  act(() => { expect(registry().acceptDraft(from, {
    conversation: { id: CONVERSATION, trackId: 'track', kind: 'track-assistant', provider: 'codex', title: null, state: 'idle', updatedAt: 1 },
    initialInput: { kind: 'known', receipt: { version: 'initial-input-v1', entryId: 'old-entry', queuedAtMs: 10,
      textSha256: sha256Hex(submitted.text), initialRuntimeId: 'runtime' } },
  }, submitted)).toBe(false); });
  expect(registry().draftOf(from.scopeId)).toMatchObject({ key: 'new-key', text: 'new words', submitted: null });
  expect(registry().outboxOf(CONVERSATION)).toEqual([]);
});
