import { defineConfig } from @playwright/test;

export default defineConfig({
  testDir: ./,
  testMatch: nexus-ui-smoke.spec.ts,
  retries: 0,
  timeout: 60_000,
  use: {
    baseURL: http://127.0.0.1:3000,
    trace: off,
  },
  projects: [{ name: chromium, use: { channel: undefined } }],
});
