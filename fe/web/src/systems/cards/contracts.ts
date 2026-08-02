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
  set<Value>(key: string, value: Value): void;
}

export interface CardHostCapabilities {
  readonly cardId: string;
  readonly lifecycle: CardLifecycleStore;
  readonly slots: CardSlotStore;
  emit(command: CardRuntimeCommand): void;
}

export type CreateCardController = (
  card: CardRecord,
  host: CardHostCapabilities,
) => CardController;
