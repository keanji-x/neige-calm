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

export type CardControllerCallback = 'onVisibleChange' | 'onFocusChange' | 'onResize' | 'onRefresh';

export interface CardControllerErrorContext {
  readonly cardId: string;
  readonly callback: CardControllerCallback;
}

export interface CardHostOptions {
  onControllerError?(error: unknown, context: CardControllerErrorContext): void;
}

function connectController(
  writer: CardLifecycleWriter,
  controller: CardController,
  cardId: string,
  onControllerError: NonNullable<CardHostOptions['onControllerError']>,
): () => void {
  const deliver = (callback: CardControllerCallback, result: void | Promise<void>): void => {
    void Promise.resolve(result).catch((error: unknown) => onControllerError(error, { cardId, callback }));
  };
  let previous = writer.getSnapshot();
  return writer.subscribe(() => {
    const current = writer.getSnapshot();
    const delivered = previous;
    previous = current;
    if (current.visible !== delivered.visible && controller.onVisibleChange !== undefined) {
      deliver('onVisibleChange', controller.onVisibleChange(current.visible));
    }
    if (current.focused !== delivered.focused && controller.onFocusChange !== undefined) {
      deliver('onFocusChange', controller.onFocusChange(current.focused));
    }
    if (!sameGeometry(current.geometry, delivered.geometry) && controller.onResize !== undefined) {
      deliver('onResize', controller.onResize(current.geometry));
    }
    if (current.refreshEpoch > delivered.refreshEpoch && controller.onRefresh !== undefined) {
      deliver('onRefresh', controller.onRefresh());
    }
  });
}

export function createCardHost(registry: CardRegistry, options: CardHostOptions = {}): CardHost {
  const resolver = createCardInstanceResolver();
  const onControllerError = options.onControllerError === undefined
    ? ((error: unknown, context: CardControllerErrorContext) => {
      console.error('CardControllerCallbackRejected', context, error);
    })
    : (error: unknown, context: CardControllerErrorContext) => {
      options.onControllerError?.(error, context);
    };
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
      function getSlot<Value>(key: string): Value | undefined;
      function getSlot<Value>(key: string, initial: Value | (() => Value)): Value;
      function getSlot<Value>(key: string, initial?: Value | (() => Value)): Value | undefined {
        if (!slotValues.has(key)) {
          const value = typeof initial === 'function' ? (initial as () => Value)() : initial;
          slotValues.set(key, value);
          slotInitials.set(key, value);
        } else if (
          import.meta.env.DEV
          && initial !== undefined
          && typeof initial !== 'function'
          && !Object.is(slotInitials.get(key), initial)
        ) {
          console.warn(
            `CardSlotInitialConflict(${key}): first=${String(slotInitials.get(key))}, next=${String(initial)}`,
          );
        }
        return slotValues.get(key) as Value | undefined;
      }
      const slots: CardSlotStore = Object.freeze({
        get: getSlot,
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
      const entry = registry.get(card.type);
      let controller: CardController | undefined;
      let disconnect = (): void => undefined;
      try {
        controller = entry?.createController?.(card, capabilities);
        if (entry?.refreshBacking === 'epoch' && controller?.onRefresh !== undefined) {
          throw new Error(`RefreshBackingConflict(${entry.type})`);
        }
        if (controller !== undefined) {
          disconnect = connectController(writer, controller, card.id, onControllerError);
        }
      } catch (error) {
        disconnect();
        void controller?.dispose?.();
        throw error;
      }
      const unregister = resolver.register(card.id, capabilities);
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
