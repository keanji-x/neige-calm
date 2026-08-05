import { describe, expect, it } from 'vitest';
import { retryUnless401 } from './public.tsx';

describe('app/providers contracts', () => {
  it('INV-APP-059 INV-APP-060 never retries 401 and retries other failures once', () => {
    expect(retryUnless401(0, { kind: 'unauthorized', status: 401 })).toBe(false);
    expect(retryUnless401(0, { kind: 'http', status: 500 })).toBe(true);
    expect(retryUnless401(1, new Error('network'))).toBe(false);
  });
});
