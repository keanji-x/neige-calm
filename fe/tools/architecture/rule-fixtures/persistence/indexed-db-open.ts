(globalThis as unknown as { indexedDB: { open(name: string): unknown } }).indexedDB.open('database');
