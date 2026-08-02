declare const persistentBrand: unique symbol;

/** Compile-time-only marker for values whose lifecycle belongs to persistent storage. */
export type Persistent<T> = T & { readonly [persistentBrand]: true };

/** Value-or-updater shape shared by React state and overlay state consumers. */
export type StateUpdate<T> = T | ((previous: T) => T);

/** The frozen overlay consumer shape intentionally has no loading member. */
export type OverlayState<T> = readonly [
  Persistent<T>,
  (update: StateUpdate<T>) => void,
];

export type DecodeFailure = Readonly<{
  kind: 'decode';
  message: string;
  cause?: unknown;
}>;

export type DecodeResult<T> =
  | Readonly<{ status: 'decoded'; value: T }>
  | Readonly<{ status: 'failed'; error: DecodeFailure }>;

/** A codec reports malformed persisted data as data; it does not throw it across the port. */
export interface Codec<T, Encoded = string> {
  encode(value: T): Encoded;
  decode(encoded: Encoded): DecodeResult<T>;
}

export type OverlayKey<
  PluginId extends string = string,
  EntityKind extends string = string,
  EntityId extends string = string,
  Kind extends string = string,
> = readonly ['overlay', PluginId, EntityKind, EntityId, Kind];

export type OverlayMutation<T> = Readonly<{
  previous: Persistent<T>;
  next: Persistent<T>;
}>;

export type OverlayPersistResult =
  | Readonly<{ status: 'persisted' }>
  | Readonly<{ status: 'failed'; error: Readonly<{ kind: 'write'; message: string; cause?: unknown }> }>;

/**
 * End-side assembly implements optimistic ordering. updateSynchronously must cancel an in-flight
 * read before replacing cached data and return the per-call snapshot used by persist/rollback.
 */
export interface OverlayStatePort<T> {
  read(key: OverlayKey): Promise<Persistent<T> | undefined>;
  updateSynchronously(key: OverlayKey, update: StateUpdate<T>): OverlayMutation<T>;
  persist(key: OverlayKey, mutation: OverlayMutation<T>): Promise<OverlayPersistResult>;
  rollback(key: OverlayKey, mutation: OverlayMutation<T>): void;
}

export function createOverlayKey<
  PluginId extends string,
  EntityKind extends string,
  EntityId extends string,
  Kind extends string,
>(
  pluginId: PluginId,
  entityKind: EntityKind,
  entityId: EntityId,
  kind: Kind,
): OverlayKey<PluginId, EntityKind, EntityId, Kind> {
  return ['overlay', pluginId, entityKind, entityId, kind];
}

export type StorageReadFailure = Readonly<{
  kind: 'read';
  message: string;
  cause?: unknown;
}>;

export type StorageWriteFailure = Readonly<{
  kind: 'write' | 'quota-exceeded';
  message: string;
  cause?: unknown;
}>;

export type StorageReadResult<T> =
  | Readonly<{ status: 'missing' }>
  | Readonly<{ status: 'ready'; value: T }>
  | Readonly<{ status: 'failed'; error: StorageReadFailure | DecodeFailure }>;

export type StorageWriteResult =
  | Readonly<{ status: 'stored' }>
  | Readonly<{ status: 'failed'; error: StorageWriteFailure }>;

export type StorageRemoveResult =
  | Readonly<{ status: 'removed' }>
  | Readonly<{ status: 'failed'; error: StorageWriteFailure }>;

/**
 * Async storage boundary: adapters own platform access and map exceptions into explicit results.
 * Missing data is a normal initial lifecycle state; failures never masquerade as missing data.
 */
export interface StateStoragePort {
  read<T>(key: string, codec: Codec<T>): Promise<StorageReadResult<T>>;
  write<T>(key: string, value: T, codec: Codec<T>): Promise<StorageWriteResult>;
  remove(key: string): Promise<StorageRemoveResult>;
}

/** Apply the phantom brand without changing identity or runtime representation. */
export function asPersistent<T>(value: T): Persistent<T> {
  return value as Persistent<T>;
}
