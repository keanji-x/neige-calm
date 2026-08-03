import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    projects: [
      {
        test: {
          name: 'platform-independent',
          environment: 'node',
          include: ['core/**/*.test.ts', 'tools/**/*.test.ts', 'web/src/**/*.test.{ts,tsx}'],
          exclude: ['web/src/ui/**/*.test.{ts,tsx}'],
        },
      },
      {
        test: {
          name: 'ui-dom',
          environment: 'jsdom',
          include: ['web/src/ui/**/*.test.{ts,tsx}'],
        },
      },
    ],
  },
});
