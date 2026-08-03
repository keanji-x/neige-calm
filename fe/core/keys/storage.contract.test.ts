import { describe, expectTypeOf, it } from 'vitest';

import type {
  Codec,
  StorageReadResult,
  StorageRemoveResult,
  StorageWriteResult,
} from '../state/types.js';
import type { StorageAdapterPort, StorageKey } from './storage.js';

describe('core/keys storage public contract', () => {
  it('brands keys and exposes only injected adapter operations', () => {
    expectTypeOf<StorageKey>().toMatchTypeOf<string>();
    expectTypeOf<string>().not.toMatchTypeOf<StorageKey>();
    expectTypeOf<StorageAdapterPort['read']>().toEqualTypeOf<
      <T>(key: StorageKey, codec: Codec<T>) => Promise<StorageReadResult<T>>
    >();
    expectTypeOf<StorageAdapterPort['write']>().toEqualTypeOf<
      <T>(key: StorageKey, value: T, codec: Codec<T>) => Promise<StorageWriteResult>
    >();
    expectTypeOf<StorageAdapterPort['remove']>().toEqualTypeOf<
      (key: StorageKey) => Promise<StorageRemoveResult>
    >();

    const compileOnly = false as boolean;
    if (compileOnly) {
      // @ts-expect-error -- INV-APP-017: literal keys must come from the storage key factory.
      const literalKey: StorageKey = 'calm:sync:cursor';
      const adapter = null as unknown as StorageAdapterPort;
      const arbitraryKey = 'calm:arbitrary' as string;
      // @ts-expect-error -- INV-APP-017: the adapter rejects unbranded strings.
      void adapter.read(arbitraryKey, null as unknown as Codec<unknown>);
      void literalKey;
    }
  });
});
