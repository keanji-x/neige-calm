import { describe, expect, it } from 'vitest';

import { createRecentFileHistory } from './recent-files.ts';

function memoryStorage(initial: Readonly<Record<string, string>> = {}) {
  const values = new Map(Object.entries(initial));
  return {
    values,
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => { values.set(key, value); },
  };
}

describe('recent file history', () => {
  it('is per-track, newest-first, de-duplicated, and bounded to eight files', () => {
    const history = createRecentFileHistory(memoryStorage());
    for (let index = 0; index < 10; index += 1) {
      history.record('track:a', `src/${index}.ts`);
    }
    expect(history.read('track:a')).toEqual([
      'src/9.ts', 'src/8.ts', 'src/7.ts', 'src/6.ts',
      'src/5.ts', 'src/4.ts', 'src/3.ts', 'src/2.ts',
    ]);
    expect(history.record('track:a', 'src/5.ts')[0]).toBe('src/5.ts');
    expect(history.read('track:b')).toEqual([]);
  });

  it('restores valid persisted rows and drops malformed or unsafe values', () => {
    const storage = memoryStorage();
    const first = createRecentFileHistory(storage);
    first.record('w1', 'src/a.ts');
    first.record('w1', 'README.md');

    const restored = createRecentFileHistory(storage);
    expect(restored.read('w1')).toEqual(['README.md', 'src/a.ts']);

    const [key] = [...storage.values.keys()];
    if (key === undefined) throw new Error('recent file key was not persisted');
    storage.values.set(key, JSON.stringify(['ok.ts', '../escape', '/etc/passwd', 3, 'ok.ts']));
    expect(createRecentFileHistory(storage).read('w1')).toEqual(['ok.ts']);
  });

  it('keeps an in-memory history when browser storage throws', () => {
    const history = createRecentFileHistory({
      getItem: () => { throw new Error('blocked'); },
      setItem: () => { throw new Error('quota'); },
    });
    expect(history.record('w1', 'src/a.ts')).toEqual(['src/a.ts']);
    expect(history.read('w1')).toEqual(['src/a.ts']);
  });
});
