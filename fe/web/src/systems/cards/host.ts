import {
  createCardLifecycleStore,
  sameGeometry,
} from './lifecycle.js';
import type {
  CardController,
  CardGeometry,
  CardHostCapabilities,
  CardLifecycleWriter,
  CardRuntimeCommand,
  CardSlotStore,
} from './contracts.js';
import type { CardRegistry, RegisteredCard } from './registry.js';
import { createCardInstanceResolver } from './resolver.js';

export interface CardHostWriter {
  setVisible(visible: boolean): void;
  setFocused(focused: boolean): void;
  setGeometry(geometry: CardGeometry): void;
}

export interface MountedCard {
  readonly card: CardHostCapabilities;
  readonly host: CardHostWriter;
  unmount(): void;
}

export interface CardHost {
  readonly registry: CardRegistry;
  mount(card: RegisteredCard): MountedCard;
  resolve(cardId: string): CardHostCapabilities | null;
}

function connectController(writer: CardLifecycleWriter, controller: CardController): () => void {
  let previous = writer.getSnapshot();
  return writer.subscribe(() => {
    const current = writer.getSnapshot();
    if (current.visible !== previous.visible) void controller.onVisibleChange?.(current.visible);
    if (current.focused !== previous.focused) void controller.onFocusChange?.(current.focused);
    if (!sameGeometry(current.geometry, previous.geometry)) void controller.onResize?.(current.geometry);
    if (current.refreshEpoch > previous.refreshEpoch) void controller.onRefresh?.();
    previous = current;
  });
}

export function createCardHost(registry: CardRegistry): CardHost {
  const resolver = createCardInstanceResolver();
  return Object.freeze({
    registry,
    mount(card: RegisteredCard): MountedCard {
      const writer = createCardLifecycleStore();
      const lifecycle = Object.freeze({
        getSnapshot: () => writer.getSnapshot(),
        subscribe: (listener: () => void) => writer.subscribe(listener),
      });
      const slotValues = new Map<string, unknown>();
      const slotInitials = new Map<string, unknown>();
      const slots: CardSlotStore = Object.freeze({
        get: <Value>(key: string, initial?: Value | (() => Value)) => {
          if (!slotValues.has(key)) {
            const value = typeof initial === 'function' ? (initial as () => Value)() : initial;
            slotValues.set(key, value);
            slotInitials.set(key, initial);
          } else if (import.meta.env.DEV && initial !== undefined && !Object.is(slotInitials.get(key), initial)) {
            console.warn(`CardSlotInitialConflict(${key}): first initial differs from later initial`);
          }
          return slotValues.get(key) as Value;
        },
        set: <Value>(key: string, value: Value) => { slotValues.set(key, value); },
      });
      const capabilities: CardHostCapabilities = Object.freeze({
        cardId: card.id,
        lifecycle,
        slots,
        emit(command: CardRuntimeCommand) {
          if (command.type === 'refresh') writer.bumpRefresh();
        },
      });
      const unregister = resolver.register(card.id, capabilities);
      const entry = registry.get(card.type);
      const controller = entry?.createController?.(card, capabilities);
      if (entry?.refreshBacking === 'epoch' && controller?.onRefresh !== undefined) {
        throw new Error(`RefreshBackingConflict(${entry.type})`);
      }
      const disconnect = controller === undefined ? () => undefined : connectController(writer, controller);
      let mounted = true;
      return Object.freeze({
        card: capabilities,
        host: Object.freeze({
          setVisible: (visible: boolean) => writer.setVisible(visible),
          setFocused: (focused: boolean) => writer.setFocused(focused),
          setGeometry: (geometry: CardGeometry) => writer.setGeometry(geometry),
        }),
        unmount() {
          if (!mounted) return;
          mounted = false;
          disconnect();
          unregister();
          void controller?.dispose?.();
        },
      });
    },
    resolve: (cardId: string) => resolver.resolve(cardId),
  });
}

export type { CardHostCapabilities, CardSlotStore } from './contracts.js';
