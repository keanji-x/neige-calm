import type {
  Codec,
  StorageReadResult,
  StorageRemoveResult,
  StorageWriteResult,
} from '../state/types.js';

declare const storageKeyBrand: unique symbol;

export type StorageKey = string & { readonly [storageKeyBrand]: true };

/** Shared with the legacy query persister until that persisted cache is retired. */
export const IDB_DB_NAME = 'neige-calm';
export const SYNC_CURSOR_KEY = 'calm:sync:cursor' as StorageKey;
export const DB_INSTANCE_ID_KEY = 'calm:db_instance_id' as StorageKey;
export const THEME_KEY = 'calm.theme' as StorageKey;

/** Creates application-owned keys; segments must already be normalized and non-empty. */
export function createStorageKey(...segments: readonly [string, ...string[]]): StorageKey {
  if (segments.some((segment) => segment.length === 0 || segment.includes(':'))) {
    throw new TypeError('Storage key segments must be non-empty and may not contain colons');
  }
  return `calm:${segments.join(':')}` as StorageKey;
}

/** Key-branded storage adapter consumed by session, event, and application assembly. */
export interface StorageAdapterPort {
  read<T>(key: StorageKey, codec: Codec<T>): Promise<StorageReadResult<T>>;
  write<T>(key: StorageKey, value: T, codec: Codec<T>): Promise<StorageWriteResult>;
  remove(key: StorageKey): Promise<StorageRemoveResult>;
}
