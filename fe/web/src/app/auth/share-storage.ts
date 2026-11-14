/** A Storage view whose enumeration and clear cannot touch another session's keys. */
export function createShareStorage(storage: Storage, shareId: string): Storage {
  const prefix = `calm-share:${shareId}:`;
  const keys = () => Array.from({ length: storage.length }, (_, index) => storage.key(index))
    .filter((key): key is string => key !== null && key.startsWith(prefix));
  return {
    get length() { return keys().length; },
    key: index => keys()[index]?.slice(prefix.length) ?? null,
    getItem: key => storage.getItem(prefix + key),
    setItem: (key, value) => storage.setItem(prefix + key, value),
    removeItem: key => storage.removeItem(prefix + key),
    clear: () => { for (const key of keys()) storage.removeItem(key); },
  };
}
