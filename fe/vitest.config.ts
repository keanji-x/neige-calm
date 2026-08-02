import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    environment: 'node',
    include: ['core/**/*.test.ts', 'tools/**/*.test.ts', 'web/src/**/*.test.ts'],
  },
});
