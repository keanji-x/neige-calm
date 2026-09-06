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
    const echo = { id: 'echo-1', author: 'you' as const, text: 'do this', atMs: 0, serverHighWaterBefore: 5 };
    expect(hasUnseenMatchingConversationMessage([{ id: '6:0', author: 'you', text: 'do this', atMs: 1 }], echo)).toBe(true);
    expect(hasUnseenMatchingConversationMessage([{ id: '6', author: 'agent', text: 'do this', atMs: 1 }], echo)).toBe(false);
    expect(hasUnseenMatchingConversationMessage([{ id: '5', author: 'you', text: 'do this', atMs: 1 }], echo)).toBe(false);
    expect(hasUnseenMatchingConversationMessage([{ id: '6', author: 'you', text: 'do this\nand that', atMs: 1 }], echo)).toBe(false);
    expect(hasUnseenMatchingConversationMessage([{ id: '6', author: 'you', text: '', atMs: 1 }], { ...echo, text: '' })).toBe(false);
  });
});
