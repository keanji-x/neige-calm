import { expect, it, vi } from 'vitest';
import { createConversationOwner } from './registry-owner.ts';
import type { ConversationDraft } from './registry-model.ts';
import type { Conversation } from '../../../../core/domain/conversation.ts';
import type { SendOp } from '../../../../core/domain/conversation-outbox.ts';
import { sha256Hex } from '../../../../core/domain/sha256.ts';
import { EMPTY_COMPOSER, isComposerEmpty } from '../../../../core/domain/conversation-composer.ts';

const card = 'opaque:card';
const draft: ConversationDraft = { scopeId: 'opaque:scope', provider: 'codex', model: { model: null, reasoning_effort: null },
  key: 'attempt', text: 'Hello', submitted: null, creating: false, error: null, remedy: null };
const conversation: Conversation = { id: card, trackId: 'track', title: null, kind: 'track-assistant', provider: 'codex', state: 'idle', updatedAt: 1 };
const sending = (key: string): SendOp => ({ key, phase: 'sending', unknown: false, fromComposer: true, replaces: null,
  echo: { id: `echo-${key}`, author: 'you', text: key, atMs: 1, attachments: [], serverHighWaterBefore: 0, queued: false, entryId: null } });

it('notifies only changed selected facts and preserves no-op, empty and summary snapshots', () => {
  const owner = createConversationOwner();
  const registry = owner.commands;
  const text = vi.fn(); const other = vi.fn(); const upload = vi.fn(); const empty = vi.fn(); const summary = vi.fn();
  owner.subscribe(state => state.composers[card]?.text ?? '', text);
  owner.subscribe(state => state.composers.other?.text ?? '', other);
  owner.subscribe(state => state.uploads[card], upload);
  owner.subscribe(state => isComposerEmpty(state.composers[card] ?? EMPTY_COMPOSER), empty);
  owner.subscribe(state => state.conversations, summary);
  registry.editComposer(card, current => ({ ...current, text: 'First' }));
  registry.editComposer(card, current => ({ ...current, text: 'First words' }));
  expect(text).toHaveBeenCalledTimes(2); expect(empty).toHaveBeenCalledOnce();
  expect(other).not.toHaveBeenCalled(); expect(upload).not.toHaveBeenCalled(); expect(summary).not.toHaveBeenCalled();
  const before = owner.getSnapshot();
  registry.editComposer(card, current => current); registry.nextRead();
  expect(owner.getSnapshot()).toBe(before);
  registry.remember(conversation, []);
  const rows = owner.getSnapshot().conversations;
  registry.remember({ ...conversation }, [{ id: 'message', author: 'agent', text: 'Reply', atMs: 2 }]);
  expect(owner.getSnapshot().conversations).toBe(rows);
  expect(summary).toHaveBeenCalledOnce();
  const turns = registry.rememberedTurnsOf(card);
  registry.remember({ ...conversation, title: 'Named' }, [...turns]);
  expect(registry.rememberedTurnsOf(card)).toBe(turns);
  expect(summary).toHaveBeenCalledTimes(2);
  registry.editComposer(card, current => ({ ...current, text: ' \n ' }));
  expect(empty).toHaveBeenCalledTimes(2);
});

it('preserves explicit repeated open intents and exact idempotent unsubscribe', () => {
  const owner = createConversationOwner(); const registry = owner.commands;
  const observe = vi.fn(); const unsubscribe = owner.subscribe(state => state.openRequest, observe);
  registry.requestOpen(card, { focusComposer: true });
  const first = owner.getSnapshot().openRequest;
  registry.requestOpen(card, { focusComposer: true });
  expect(owner.getSnapshot().openRequest).toEqual(first); expect(owner.getSnapshot().openRequest).not.toBe(first);
  expect(observe).toHaveBeenCalledTimes(2);
  unsubscribe(); unsubscribe();
  registry.clearOpenRequest();
  expect(observe).toHaveBeenCalledTimes(2);
});

it('uses current upload ownership when cancelling an edit in the same tick', () => {
  const registry = createConversationOwner().commands;
  registry.beginEdit(card, { kind: 'turn', turnId: 'turn', outcomeId: 'outcome', refill: { text: 'Retained prompt', attachments: [] } });
  registry.editUpload(card, () => ({ inFlight: 1, refusal: null }));
  registry.cancelEdit(card);
  expect(registry.editOf(card)).toBeNull();
  expect(registry.composerOf(card).text).toBe('Retained prompt');
});

it('publishes edit refill and send departure atomically before synchronous observers and fences a same-tick send', () => {
  const owner = createConversationOwner(); const registry = owner.commands;
  const edit = { kind: 'turn', turnId: 'turn', outcomeId: 'outcome', refill: { text: 'Original', attachments: [] } } as const;
  const observed: unknown[] = [];
  owner.subscribe(state => state, () => { observed.push({ edit: registry.editOf(card), composer: registry.composerOf(card), outbox: registry.outboxOf(card), notice: registry.editNoticeOf(card) }); });
  registry.noteRefusedSend(card, { kind: 'refused', edit: true, message: 'Old refusal' });
  observed.length = 0;
  expect(registry.beginEdit(card, edit)).toBe(true);
  expect(observed).toHaveLength(1);
  expect(observed[0]).toMatchObject({ edit: { kind: 'turn' }, composer: { text: 'Original' }, notice: null });
  const op = { ...sending('one'), replaces: { turnId: 'turn', outcomeId: 'outcome' } };
  observed.length = 0;
  expect(registry.beginSend(card, op)).toBe(true);
  expect(registry.beginSend(card, sending('two'))).toBe(false);
  expect(observed).toHaveLength(1);
  expect(observed[0]).toMatchObject({ edit: null, notice: null, outbox: [{ key: 'one' }] });
});

it('publishes accepted delivery and adoption together; stale keys and exceptions publish nothing', () => {
  const owner = createConversationOwner(); const registry = owner.commands;
  const submitted = { text: 'Hello', provider: 'codex', model: { model: null, reasoning_effort: null }, atMs: 2 } as const;
  const receipt = { kind: 'known', receipt: { version: 'initial-input-v1', entryId: 'first', entryRev: 0, queuedAtMs: 3, textSha256: sha256Hex('Hello'), initialRuntimeId: 'runtime' } } as const;
  registry.startDraft(draft);
  const observations: unknown[] = [];
  owner.subscribe(state => state, () => { observations.push({ held: registry.draftOf(draft.scopeId), adopted: registry.adoptedDraftIdOf(draft.scopeId), delivery: registry.outboxOf(card) }); });
  const before = owner.getSnapshot();
  expect(() => registry.editDraft(draft, () => { throw new Error('bad updater'); })).toThrow('bad updater');
  expect(() => registry.acceptDraft(draft, { conversation, initialInput: { ...receipt, receipt: { ...receipt.receipt, textSha256: 'mismatch' } } }, submitted)).toThrow();
  expect(registry.acceptDraft({ ...draft, key: 'older' }, { conversation, initialInput: receipt }, submitted)).toBe(false);
  expect(owner.getSnapshot()).toBe(before); expect(observations).toEqual([]);
  expect(registry.acceptDraft(draft, { conversation, initialInput: receipt }, submitted)).toBe(true);
  expect(observations).toHaveLength(1);
  expect(observations[0]).toMatchObject({ held: null, adopted: card, delivery: [{ phase: 'initial-accepted', key: draft.key }] });
});

it('clears spent rows in the same publication as final outbox removal and returns the written outbox', () => {
  const owner = createConversationOwner(); const registry = owner.commands;
  registry.editOutbox(card, () => [{ ...sending('first'), phase: 'confirmed', queueRevision: null }, sending('left')]);
  registry.retireSends(card, [{ key: 'first', row: 'row' }]);
  expect(registry.spentRowsOf(card)).toEqual(['row']);
  const observe = vi.fn(() => { expect(registry.outboxOf(card)).toEqual([]); expect(registry.spentRowsOf(card)).toEqual([]); });
  owner.subscribe(state => state.outboxes[card], observe);
  const written = registry.editOutbox(card, () => []);
  expect(written).toEqual([]); expect(registry.outboxOf(card)).toEqual(written); expect(observe).toHaveBeenCalledOnce();
});

it('keeps queue acquisition after its microtask, notifies only busy changes and fences synchronous restarts', async () => {
  const owner = createConversationOwner(); const registry = owner.commands;
  const busy = vi.fn(); owner.subscribe(state => (state.queueWrites[card] ?? 0) > 0, busy);
  let releaseFirst!: () => void; let releaseSecond!: () => void;
  const first = registry.holdQueueWrite(card, () => new Promise<void>(resolve => { releaseFirst = resolve; }));
  const second = registry.holdQueueWrite(card, () => new Promise<void>(resolve => { releaseSecond = resolve; }));
  expect(registry.queueWriteOutOf(card)).toBe(false);
  await Promise.resolve();
  expect(registry.queueWriteOutOf(card)).toBe(true); expect(busy).toHaveBeenCalledOnce();
  releaseFirst(); await first;
  expect(registry.queueWriteOutOf(card)).toBe(true); expect(busy).toHaveBeenCalledOnce();
  releaseSecond(); await second;
  expect(registry.queueWriteOutOf(card)).toBe(false); expect(busy).toHaveBeenCalledTimes(2);
  let releaseRestart!: () => void;
  const restart = registry.holdRestart(card, () => new Promise<void>(resolve => { releaseRestart = resolve; }));
  expect(registry.restartOutOf(card)).toBe(true);
  expect(registry.holdRestart(card, async () => {})).toBeNull();
  await Promise.resolve(); releaseRestart(); await restart;
  expect(registry.restartOutOf(card)).toBe(false);
});
