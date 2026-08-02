import { describe, expectTypeOf, it } from 'vitest';

import type {
  Codec,
  OverlayKey,
  OverlayMutation,
  OverlayState,
  OverlayStatePort,
  Persistent,
  StateStoragePort,
  StorageReadResult,
  StorageWriteResult,
} from './types.js';

describe('core/state public type contract', () => {
  it('keeps the brand phantom and exposes codec and storage error channels', () => {
    expectTypeOf<Persistent<{ count: number }>>().toMatchTypeOf<{ count: number }>();
    expectTypeOf<Codec<number>>().toHaveProperty('encode');
    expectTypeOf<Codec<number>>().toHaveProperty('decode');
    expectTypeOf<StateStoragePort>().toHaveProperty('read');
    expectTypeOf<StateStoragePort>().toHaveProperty('write');
    expectTypeOf<StateStoragePort>().toHaveProperty('remove');
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
    expectTypeOf<OverlayStatePort<number>['updateSynchronously']>().returns
      .toEqualTypeOf<OverlayMutation<number>>();
    expectTypeOf<OverlayStatePort<number>>().toHaveProperty('persist');
    expectTypeOf<OverlayStatePort<number>>().toHaveProperty('rollback');
  });
});
