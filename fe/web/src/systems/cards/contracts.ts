import type { CardFilesPort } from '../../../../core/domain/fs.ts';

export type { CardFilesPort };

export interface CardRecord {
  readonly id: string;
  readonly type: string;
}

export interface CardGeometry {
  readonly width: number;
  readonly height: number;
  readonly ready: boolean;
}

export interface CardLifecycleSnapshot {
  readonly visible: boolean;
  readonly focused: boolean;
  readonly geometry: CardGeometry;
  readonly refreshEpoch: number;
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

export type CardRuntimeCommand = Readonly<{ type: 'refresh' }>;

export interface CardController {
  onVisibleChange?(visible: boolean): void | Promise<void>;
  onFocusChange?(focused: boolean): void | Promise<void>;
  onResize?(geometry: CardGeometry): void | Promise<void>;
  onRefresh?(): void | Promise<void>;
  dispose?(): void | Promise<void>;
}

export interface CardSlotStore {
  get<Value>(key: string): Value | undefined;
  get<Value>(key: string, initial: Value | (() => Value)): Value;
  set<Value>(key: string, value: Value): void;
}

export interface CardHostCapabilities {
  readonly cardId: string;
  readonly lifecycle: CardLifecycleStore;
  readonly slots: CardSlotStore;
  /**
   * The filesystem reads this host was built with, or `null`.
   *
   * A card cannot reach a transport on its own — `systems/**` sits below
   * `app/**` and holds none — so the reads arrive here, injected once by
   * `app/composition`. `null` is a host assembled without them (a test, a
   * board that hosts no file-reading kind), and a card that needs them must
   * say so on screen rather than throw: an empty pane with a reason is a state
   * a reader can act on, an exception is not.
   */
  readonly files: CardFilesPort | null;
  emit(command: CardRuntimeCommand): void;
}

export type CreateCardController = (
  card: CardRecord,
  host: CardHostCapabilities,
) => CardController;
