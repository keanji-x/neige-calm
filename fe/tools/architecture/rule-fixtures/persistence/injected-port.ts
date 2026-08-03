type StoragePort = { get(key: string): string | null };
export const load = (storage: StoragePort): string | null => storage.get('key');
