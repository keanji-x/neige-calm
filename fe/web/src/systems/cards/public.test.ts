import { describe, expect, it, vi } from 'vitest';

import type { CardEntry } from './public.js';
import { createCardHost, createCardRegistry } from './public.js';

declare module './registry.js' {
  interface CardDataMap {
    behaviorOne: { type: 'behavior-one'; id: string; payload: { label: string } };
    behaviorTwo: { type: 'behavior-two'; id: string; payload: { label: string } };
  }
}

const base = {
  component: () => null,
  defaultSize: Object.freeze({ w: 2, h: 3, minW: 1, minH: 1 }),
  title: () => 'title',
  accessibleName: () => 'accessible',
  create: Object.freeze({ mode: 'kernel-minted-only' } as const),
};

describe('cards public behavior', () => {
  it('runs the public-only fake consumer chain: register, resolve, host, lifecycle', () => {
    const registry = createCardRegistry();
    const visible = vi.fn();
    const resized = vi.fn();
    const refreshed = vi.fn();
    const disposed = vi.fn();
    const first: CardEntry = {
      ...base,
      type: 'behavior-one',
      claim: { mode: 'exact', kind: 'literal-one' },
      fromKernel: (raw) => raw.kind === 'literal-one'
        ? { type: 'behavior-one', id: raw.id, payload: { label: 'one' } }
        : null,
    };
    const second: CardEntry = {
      ...base,
      type: 'behavior-two',
      claim: { mode: 'prefix', prefix: 'literal://' },
      fromKernel: (raw) => raw.kind.startsWith('literal://')
        ? { type: 'behavior-two', id: raw.id, payload: { label: 'two' } }
        : null,
      createController: () => ({
        onVisibleChange: visible,
        onResize: resized,
        onRefresh: refreshed,
        dispose: disposed,
      }),
    };
    registry.register(first);
    registry.register(second);
    const card = registry.resolve({ id: 'card-7', kind: 'literal://view', payload: {} });
    if (card === null) throw new Error('expected fake card to resolve');

    const host = createCardHost(registry);
    const mounted = host.mount(card);
    expect(host.resolve('card-7')).toBe(mounted.card);
    const snapshots: boolean[] = [];
    const unsubscribe = mounted.card.lifecycle.subscribe(() => {
      snapshots.push(mounted.card.lifecycle.getSnapshot().visible);
    });
    mounted.host.setVisible(false);
    mounted.host.setGeometry({ width: 640, height: 480, ready: true });
    mounted.card.emit({ type: 'refresh' });
    mounted.card.slots.set('selection', 'row-1');
    expect(mounted.card.slots.get<string>('selection')).toBe('row-1');
    expect(snapshots).toEqual([false, false, false]);
    expect(visible).toHaveBeenCalledWith(false);
    expect(resized).toHaveBeenCalledWith({ width: 640, height: 480, ready: true });
    expect(refreshed).toHaveBeenCalledOnce();
    unsubscribe();
    mounted.unmount();
    expect(host.resolve('card-7')).toBeNull();
    expect(disposed).toHaveBeenCalledOnce();
  });

  it('validates metadata, creation strategy, and claim conflicts', () => {
    const registry = createCardRegistry();
    expect(() => registry.register({ ...base, type: 'behavior-one', title: undefined } as never))
      .toThrow('EntryMissingMetadata(behavior-one, title)');
    expect(() => registry.register({ ...base, type: 'behavior-one', create: undefined } as never))
      .toThrow('MissingCreateStrategy(behavior-one)');
    registry.register({ ...base, type: 'behavior-one', claim: { mode: 'exact', kind: 'duplicate-kind' } });
    expect(() => registry.register({ ...base, type: 'behavior-two', claim: { mode: 'exact', kind: 'duplicate-kind' } }))
      .toThrow('DuplicateExactClaim(duplicate-kind)');
  });

  it('[GATE-CARD-077/078] rejects invalid refresh backing', () => {
    const registry = createCardRegistry();
    expect(() => registry.register({ ...base, type: 'behavior-one', refreshBacking: 'controller' }))
      .toThrow('RefreshBackingMissingController(behavior-one)');
    registry.register({
      ...base, type: 'behavior-one', refreshBacking: 'epoch', createController: () => ({ onRefresh: vi.fn() }),
    });
    expect(() => createCardHost(registry).mount({ type: 'behavior-one', id: 'conflict', payload: { label: 'x' } }))
      .toThrow('RefreshBackingConflict(behavior-one)');
  });

  it('[INV-CARD-072] overwrites a repeated type registration', () => {
    const registry = createCardRegistry();
    registry.register({ ...base, type: 'behavior-one', title: () => 'first' });
    const replacement = { ...base, type: 'behavior-one' as const, title: () => 'replacement' };
    expect(() => registry.register(replacement)).not.toThrow();
    expect(registry.get('behavior-one')).toBe(replacement);
  });

  it('[INV-CARD-091] skips equal visibility updates', () => {
    const registry = createCardRegistry();
    const onVisibleChange = vi.fn();
    registry.register({ ...base, type: 'behavior-one', createController: () => ({ onVisibleChange }) });
    const mounted = createCardHost(registry).mount({ type: 'behavior-one', id: 'life', payload: { label: 'x' } });
    const notified = vi.fn();
    mounted.card.lifecycle.subscribe(notified);
    mounted.host.setVisible(true);
    expect(onVisibleChange).not.toHaveBeenCalled();
    expect(notified).not.toHaveBeenCalled();
  });

  it('[INV-CARD-093] snapshots listeners before notification', () => {
    const registry = createCardRegistry();
    registry.register({ ...base, type: 'behavior-one' });
    const mounted = createCardHost(registry).mount({ type: 'behavior-one', id: 'life', payload: { label: 'x' } });
    const second = vi.fn();
    let unsubscribeSecond: () => void = () => undefined;
    mounted.card.lifecycle.subscribe(() => unsubscribeSecond());
    unsubscribeSecond = mounted.card.lifecycle.subscribe(second);
    mounted.host.setVisible(false);
    expect(second).toHaveBeenCalledOnce();
  });

  it('[INV-CARD-101] stale unmount does not unregister a replacement instance', () => {
    const registry = createCardRegistry();
    registry.register({ ...base, type: 'behavior-one' });
    const host = createCardHost(registry);
    const first = host.mount({ type: 'behavior-one', id: 'same', payload: { label: 'first' } });
    const replacement = host.mount({ type: 'behavior-one', id: 'same', payload: { label: 'replacement' } });
    first.unmount();
    expect(host.resolve('same')).toBe(replacement.card);
  });

  const mountedSlots = () => {
    const registry = createCardRegistry();
    registry.register({ ...base, type: 'behavior-one' });
    return createCardHost(registry)
      .mount({ type: 'behavior-one', id: 'slots', payload: { label: 'x' } }).card.slots;
  };

  it('[INV-CARD-086] uses initial only on the first read', () => {
    const slots = mountedSlots();
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
    expect(slots.get('first-wins', 'first')).toBe('first');
    expect(slots.get('first-wins', 'later')).toBe('first');
    warn.mockRestore();
  });

  it('[INV-CARD-087] evaluates a lazy slot initial only once', () => {
    const slots = mountedSlots();
    const lazy = vi.fn(() => ({ current: null as null | string }));
    const initialRef = slots.get('xtermRef', lazy);
    expect(slots.get('xtermRef', lazy)).toBe(initialRef);
    expect(lazy).toHaveBeenCalledOnce();
  });

  it('[INV-CARD-090] warns when a later slot initial differs', () => {
    const slots = mountedSlots();
    expect(slots.get('first-wins', 'first')).toBe('first');
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
    expect(slots.get('first-wins', 'later')).toBe('first');
    expect(warn).toHaveBeenCalledWith(expect.stringContaining('CardSlotInitialConflict(first-wins)'));
    warn.mockRestore();
  });
});
