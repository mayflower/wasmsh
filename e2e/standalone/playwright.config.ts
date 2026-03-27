import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./tests",
  timeout: 60_000,
  retries: 0,
  use: {
    baseURL: "http://localhost:3100",
  },
  webServer: {
    command: "npx serve fixture -l 3100 --no-clipboard",
    port: 3100,
    reuseExistingServer: !process.env.CI,
  },
});
