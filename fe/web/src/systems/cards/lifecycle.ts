import type { RefObject } from 'react';
import type {
  CardGeometry,
  CardHostCapabilities,
  CardLifecycleSnapshot,
  CardLifecycleWriter,
  CreateCardController,
} from './contracts.js';
export type CardWheelTargetDecl =
  | Readonly<{ kind: 'xterm'; ref: RefObject<unknown> }>
  | Readonly<{ kind: 'native-scroll'; ref: RefObject<unknown> }>
  | Readonly<{ kind: 'sink' }>;

declare module './registry.js' {
  interface CardEntry {
    readonly createController?: CreateCardController;
    readonly wheelTarget?: (
      card: RegisteredCard,
      instance: Pick<CardHostCapabilities, 'cardId' | 'slots'>,
    ) => CardWheelTargetDecl | null;
    readonly refreshBacking?: 'controller' | 'epoch' | 'none';
  }
}

function freezeSnapshot(snapshot: CardLifecycleSnapshot): CardLifecycleSnapshot {
  return Object.freeze({ ...snapshot, geometry: Object.freeze({ ...snapshot.geometry }) });
}

export function sameGeometry(left: CardGeometry, right: CardGeometry): boolean {
  return left.width === right.width && left.height === right.height && left.ready === right.ready;
}

export function createCardLifecycleStore(): CardLifecycleWriter {
  let snapshot = freezeSnapshot({
    visible: true,
    focused: false,
    geometry: { width: 0, height: 0, ready: false },
    refreshEpoch: 0,
  });
  const listeners = new Set<() => void>();
  const notify = (next: CardLifecycleSnapshot): void => {
    snapshot = freezeSnapshot(next);
    for (const listener of Array.from(listeners)) listener();
  };
  return Object.freeze({
    getSnapshot: () => snapshot,
    subscribe(listener: () => void) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    setVisible(visible: boolean) {
      if (snapshot.visible !== visible) notify({ ...snapshot, visible });
    },
    setFocused(focused: boolean) {
      if (snapshot.focused !== focused) notify({ ...snapshot, focused });
    },
    setGeometry(geometry: CardGeometry) {
      if (!sameGeometry(snapshot.geometry, geometry)) notify({ ...snapshot, geometry });
    },
    bumpRefresh() {
      notify({ ...snapshot, refreshEpoch: snapshot.refreshEpoch + 1 });
    },
  });
}

export type {
  CardController,
  CardGeometry,
  CardLifecycleSnapshot,
  CardLifecycleStore,
  CardLifecycleWriter,
  CardRuntimeCommand,
  CreateCardController,
} from './contracts.js';
