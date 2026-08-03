import { describe, expect, it } from 'vitest';

import { decodeWireEvent } from './schemas.js';

describe('core/api wire decode behavior', () => {
  it('returns unknown frames as decode data so callers can log and skip', () => {
    const result = decodeWireEvent({ ev: 'future.event', data: { version: 2 } });
    expect(result.status).toBe('failed');
    if (result.status === 'failed') expect(result.error.kind).toBe('decode');
  });
});
