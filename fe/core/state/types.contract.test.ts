import { describe, expectTypeOf, it } from 'vitest';

// @ts-expect-error -- the duplicate unbranded storage port must stay removed.
import type { StateStoragePort } from './types.js';
// @ts-expect-error -- callers must acknowledge the unsafe Persistent escape hatch by name.
import type { asPersistent } from './types.js';
import type {
  Codec,
  OverlayKey,
  OverlayMutation,
  OverlayState,
  OverlayStatePort,
  Persistent,
  StorageReadResult,
  StorageWriteResult,
} from './types.js';

describe('core/state public type contract', () => {
  void (null as unknown as StateStoragePort);
  void (null as unknown as typeof asPersistent);

  it('keeps the brand phantom and exposes codec and storage error channels', () => {
    expectTypeOf<Persistent<{ count: number }>>().toMatchTypeOf<{ count: number }>();
    expectTypeOf<Codec<number>>().toHaveProperty('encode');
    expectTypeOf<Codec<number>>().toHaveProperty('decode');
    expectTypeOf<StorageReadResult<number>['status']>().toEqualTypeOf<
      'missing' | 'ready' | 'failed'
    >();
    expectTypeOf<StorageWriteResult['status']>().toEqualTypeOf<'stored' | 'failed'>();
    expectTypeOf<Extract<StorageReadResult<number>, { status: 'failed' }>['error']['kind']>()
      .toEqualTypeOf<'read' | 'decode'>();
    expectTypeOf<Extract<StorageWriteResult, { status: 'failed' }>['error']['kind']>()
      .toEqualTypeOf<'write' | 'quota-exceeded'>();
  });

  it('freezes overlay state as a useState-shaped pair without loading state', () => {
    expectTypeOf<OverlayState<number>['length']>().toEqualTypeOf<2>();
    expectTypeOf<OverlayState<number>[0]>().toEqualTypeOf<Persistent<number>>();
    expectTypeOf<OverlayState<number>[1]>().toBeCallableWith(2);
    expectTypeOf<OverlayState<number>[1]>().toBeCallableWith((previous) => previous + 1);
  });

  it('freezes the overlay key, synchronous update, and per-call rollback snapshot ports', () => {
    expectTypeOf<OverlayKey<'plugin', 'entity', '42', 'layout'>>().toEqualTypeOf<
      readonly ['overlay', 'plugin', 'entity', '42', 'layout']
    >();
    expectTypeOf<OverlayMutation<number>>().toHaveProperty('previous');
    expectTypeOf<OverlayMutation<number>>().toHaveProperty('next');
    expectTypeOf<OverlayStatePort<number>['read']>().returns.resolves.toEqualTypeOf<
      StorageReadResult<Persistent<number>>
    >();
    expectTypeOf<OverlayStatePort<number>['updateSynchronously']>().returns
      .toEqualTypeOf<OverlayMutation<number>>();
    expectTypeOf<OverlayStatePort<number>>().toHaveProperty('persist');
    expectTypeOf<OverlayStatePort<number>>().toHaveProperty('rollback');
  });
});
