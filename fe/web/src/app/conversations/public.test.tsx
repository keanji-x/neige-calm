// @vitest-environment jsdom
// The conversation registry's composer refill and outbox bookkeeping, driven through the provider itself.

import { act, cleanup, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';

import { useEffect } from 'react';

import type { PlannerAttachment } from '../../../../core/api/generated/wire.ts';
import { MAX_ATTACHMENTS_PER_MESSAGE, TOO_MANY_IMAGES } from '../../../../core/domain/conversation.ts';
import type { SendOp } from '../../../../core/domain/conversation-outbox.ts';
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
  act(() => { registry().beginEdit(CONVERSATION, { turnId: 'turn', outcomeId: 'outcome', refill: { text: 'Prompt', attachments: turn } }); });
  expect(registry().composerOf(CONVERSATION).attachments)
    .toEqual([...held, ...turn.slice(0, MAX_ATTACHMENTS_PER_MESSAGE - held.length)]);
  expect(registry().uploadOf(CONVERSATION).refusal).toBe(TOO_MANY_IMAGES);
});

it('says nothing when an Edit’s images all fit beside the composer’s own', () => {
  const registry = mountRegistry();
  act(() => { registry().editComposer(CONVERSATION, () => ({ text: '', attachments: images('draft', 3) })); });
  act(() => { registry().beginEdit(CONVERSATION, { turnId: 'turn', outcomeId: 'outcome',
    refill: { text: 'Prompt', attachments: images('turn', MAX_ATTACHMENTS_PER_MESSAGE - 3) } }); });
  expect(registry().composerOf(CONVERSATION).attachments).toHaveLength(MAX_ATTACHMENTS_PER_MESSAGE);
  expect(registry().uploadOf(CONVERSATION).refusal).toBeNull();
});

const op = (key: string, phase: 'confirmed' | 'failed'): SendOp => {
  const echo = { id: `echo-${key}`, author: 'you', text: key, atMs: 1, attachments: [], serverHighWaterBefore: 0, queued: false,
    entryId: null } as const;
  return phase === 'confirmed'
    ? { key, echo, fromComposer: true, replaces: null, phase }
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
