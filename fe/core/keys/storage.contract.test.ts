import { describe, expectTypeOf, it } from 'vitest';

import type { StorageAdapterPort, StorageKey } from './storage.js';

describe('core/keys storage public contract', () => {
  it('brands keys and exposes only injected adapter operations', () => {
    expectTypeOf<StorageKey>().toMatchTypeOf<string>();
    expectTypeOf<StorageAdapterPort>().toHaveProperty('read');
    expectTypeOf<StorageAdapterPort>().toHaveProperty('write');
    expectTypeOf<StorageAdapterPort>().toHaveProperty('remove');
  });
});
