import { describe, expect, it } from 'vitest';

import { hasUnseenMatchingConversationMessage, failedConversationDelivery } from './conversation-delivery.js';

describe('failed conversation delivery', () => {
  it.each([400, 403, 404, 413, 422, 429])('permits an explicit %s rejection to be retried', (status) => {
    expect(failedConversationDelivery({ kind: 'http', status, code: 'rejected', message: 'rejected' })).toBe('rejected');
  });

  it.each([408, 409, 500, 502, 503, 504])('keeps HTTP %s acceptance uncertain', (status) => {
    expect(failedConversationDelivery({ kind: 'http', status, code: 'unavailable', message: 'unavailable' })).toBe('unknown');
  });

  it('keeps transport and decode failures uncertain', () => {
    expect(failedConversationDelivery({ kind: 'transport', message: 'dropped' })).toBe('unknown');
    expect(failedConversationDelivery({ kind: 'decode', message: 'malformed' })).toBe('unknown');
    expect(failedConversationDelivery(null)).toBe('unknown');
  });

  it('identifies newly observed matching text only as evidence for review', () => {
    const echo = { id: 'echo-1', author: 'you' as const, text: 'do this', atMs: 0, serverHighWaterBefore: 5, queued: false, entryId: null };
    expect(hasUnseenMatchingConversationMessage([{ id: '6:0', author: 'you', text: 'do this', atMs: 1 }], echo)).toBe(true);
    expect(hasUnseenMatchingConversationMessage([{ id: '6', author: 'agent', text: 'do this', atMs: 1 }], echo)).toBe(false);
    expect(hasUnseenMatchingConversationMessage([{ id: '5', author: 'you', text: 'do this', atMs: 1 }], echo)).toBe(false);
    expect(hasUnseenMatchingConversationMessage([{ id: '6', author: 'you', text: 'do this\nand that', atMs: 1 }], echo)).toBe(false);
    expect(hasUnseenMatchingConversationMessage([{ id: '6', author: 'you', text: '', atMs: 1 }], { ...echo, text: '' })).toBe(false);
  });

  /*
   * #1505 S6 review — an image with no words is the one message shape this
   * slice exists to add, and the text criterion answered `false` for it
   * unconditionally. The reader was therefore never told "we may already have
   * it" for exactly the message most likely to be re-sent by hand.
   */
  describe('an image with no words', () => {
    const image = (id: string) => ({
      id, contentType: 'image/png', size: 3, url: `/api/cards/c/planner/attachments/${id}`,
    });
    const echo = {
      id: 'echo-1', author: 'you' as const, text: '', atMs: 0,
      serverHighWaterBefore: 5, queued: false, attachments: [image('a.png')],
    };

    it('matches the persisted row that carries the same image', () => {
      expect(hasUnseenMatchingConversationMessage(
        [{ id: '6', author: 'you', text: '', atMs: 1, attachments: [image('a.png')] }], echo,
      )).toBe(true);
    });

    it('does not match a row carrying a different image, or one below the high-water', () => {
      expect(hasUnseenMatchingConversationMessage(
        [{ id: '6', author: 'you', text: '', atMs: 1, attachments: [image('b.png')] }], echo,
      )).toBe(false);
      expect(hasUnseenMatchingConversationMessage(
        [{ id: '5', author: 'you', text: '', atMs: 1, attachments: [image('a.png')] }], echo,
      )).toBe(false);
    });

    it('still refuses an echo that carries neither words nor images', () => {
      expect(hasUnseenMatchingConversationMessage(
        [{ id: '6', author: 'you', text: '', atMs: 1, attachments: [image('a.png')] }],
        { ...echo, attachments: [] },
      )).toBe(false);
    });
  });
});
