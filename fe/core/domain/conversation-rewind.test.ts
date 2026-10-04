import { expect, it } from 'vitest';
import { composerRefillFrom, rewindPlannerTurnOperation, withRefill } from './conversation-rewind.js';

const image = (id: string) => ({ id, contentType: 'image/png', size: 4, url: `/api/cards/c/planner/attachments/${id}` });

it('refills the composer with the prompt and steers as the transcript shows them, images in order', () => {
  const input = [
    { presentation: 'user' as const, text: 'User says:\n  Original prompt\n', attachments: [image('a.png')] },
    { presentation: 'user' as const, text: 'User says:\nA steer', attachments: [] },
    { presentation: 'user' as const, text: 'User says:\n', attachments: [image('b.png')] },
  ];
  expect(composerRefillFrom(input)).toEqual({
    text: 'Original prompt\n\nA steer',
    attachments: [image('a.png'), image('b.png')],
  });
});

it('posts only the turn id and decodes the removed input', () => {
  const operation = rewindPlannerTurnOperation('card/1', 'turn-9');
  expect(operation).toMatchObject({ method: 'POST', path: '/api/cards/card%2F1/planner/rewind', body: { turn_id: 'turn-9' } });
  expect(operation.responseSchema.parse({ card_id: 'card/1', turn_id: 'turn-9',
    input: [{ presentation: 'user', text: 'User says:\nx', attachments: [] }] }).input).toHaveLength(1);
  expect(operation.responseSchema.safeParse({ card_id: 'card/1', turn_id: 'turn-9' }).success).toBe(false);
});

it('adds a refill to a composer that already holds something, discarding nothing', () => {
  const refill = { text: 'Removed prompt', attachments: [image('a.png'), image('b.png')] };
  expect(withRefill({ text: '  ', attachments: [] }, refill)).toEqual(refill);
  expect(withRefill({ text: 'Typed', attachments: [image('a.png')] }, refill)).toEqual({
    text: 'Typed\n\nRemoved prompt', attachments: [image('a.png'), image('b.png')],
  });
  expect(withRefill({ text: 'Typed', attachments: [] }, { text: '', attachments: [image('c.png')] }))
    .toEqual({ text: 'Typed', attachments: [image('c.png')] });
});
