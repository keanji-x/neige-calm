import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./tests",
  testMatch: '**/*.spec.ts',
  testIgnore: 'built-preview.spec.ts',
  fullyParallel: false,
  workers: 1,
  use: { baseURL: "http://127.0.0.1:5194", browserName: "chromium" },
  webServer: {
    command: "npm run dev",
    url: "http://127.0.0.1:5194",
    reuseExistingServer: !process.env.CI,
  },
});
