import type { RefObject } from 'react';
import type { WaveCardData } from '../types';
import type { CardEntry, CardInstanceCtx } from './registry';

export interface CardGeometry {
  width: number;
  height: number;
  ready: boolean;
}

export interface CardLifecycleSnapshot {
  visible: boolean;
  focused: boolean;
  geometry: CardGeometry;
  refreshEpoch: number;
}

export interface CardLifecycleStore {
  getSnapshot(): CardLifecycleSnapshot;
  subscribe(listener: () => void): () => void;
}

export interface CardLifecycleWriter extends CardLifecycleStore {
  setVisible(visible: boolean): void;
  setFocused(focused: boolean): void;
  setGeometry(geometry: CardGeometry): void;
  bumpRefresh(): void;
}

export type CardRuntimeCommand = { type: 'refresh' };

export interface CardControllerContext<T extends WaveCardData = WaveCardData> {
  card: T;
  lifecycle: CardLifecycleStore;
  instance: Pick<CardInstanceCtx, 'cardId' | 'deletable' | 'useInstance'>;
  emit(cmd: CardRuntimeCommand): void;
}

export interface CardController {
  onVisibleChange?(visible: boolean): void | Promise<void>;
  onFocusChange?(focused: boolean): void | Promise<void>;
  onResize?(geometry: CardGeometry): void | Promise<void>;
  onRefresh?(): void | Promise<void>;
  dispose?(): void | Promise<void>;
}

export type CardWheelTargetDecl =
  | { kind: 'xterm'; ref: RefObject<unknown> }
  | { kind: 'native-scroll'; ref: RefObject<unknown> }
  | { kind: 'sink' };

const DEFAULT_SNAPSHOT: CardLifecycleSnapshot = freezeSnapshot({
  visible: true,
  focused: false,
  geometry: { width: 0, height: 0, ready: false },
  refreshEpoch: 0,
});

function freezeSnapshot(snapshot: CardLifecycleSnapshot): CardLifecycleSnapshot {
  return Object.freeze({
    ...snapshot,
    geometry: Object.freeze({ ...snapshot.geometry }),
  });
}

function sameGeometry(a: CardGeometry, b: CardGeometry): boolean {
  return a.width === b.width && a.height === b.height && a.ready === b.ready;
}

export function createCardLifecycleStore(
  initial: Partial<CardLifecycleSnapshot> = {},
): CardLifecycleWriter {
  let snapshot = freezeSnapshot({
    visible: initial.visible ?? DEFAULT_SNAPSHOT.visible,
    focused: initial.focused ?? DEFAULT_SNAPSHOT.focused,
    geometry: {
      width: initial.geometry?.width ?? DEFAULT_SNAPSHOT.geometry.width,
      height: initial.geometry?.height ?? DEFAULT_SNAPSHOT.geometry.height,
      ready: initial.geometry?.ready ?? DEFAULT_SNAPSHOT.geometry.ready,
    },
    refreshEpoch: initial.refreshEpoch ?? DEFAULT_SNAPSHOT.refreshEpoch,
  });
  const listeners = new Set<() => void>();

  const notify = (next: CardLifecycleSnapshot) => {
    snapshot = freezeSnapshot(next);
    for (const listener of Array.from(listeners)) listener();
  };

  return {
    getSnapshot: () => snapshot,
    subscribe(listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    setVisible(visible) {
      if (snapshot.visible === visible) return;
      notify({ ...snapshot, visible });
    },
    setFocused(focused) {
      if (snapshot.focused === focused) return;
      notify({ ...snapshot, focused });
    },
    setGeometry(geometry) {
      if (sameGeometry(snapshot.geometry, geometry)) return;
      notify({ ...snapshot, geometry });
    },
    bumpRefresh() {
      notify({ ...snapshot, refreshEpoch: snapshot.refreshEpoch + 1 });
    },
  };
}

declare module './registry' {
  interface CardEntry<
    T extends WaveCardData = WaveCardData,
    TInput = Record<string, string>,
  > {
    createController?(
      ctx: CardControllerContext<T extends WaveCardData ? T : WaveCardData>,
    ): CardController;
    wheelTarget?(
      card: T extends WaveCardData ? T : WaveCardData,
      instance: Pick<CardInstanceCtx, 'cardId' | 'useInstance'>,
    ): CardWheelTargetDecl | null;
    refreshBacking?: 'controller' | 'epoch' | 'none';
  }
  interface CardInstanceCtx {
    emit(cmd: CardRuntimeCommand): void;
  }
}

export type _CardLifecycleEntryTypeAnchor = CardEntry;
