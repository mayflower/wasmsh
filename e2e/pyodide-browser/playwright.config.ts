import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./tests",
  timeout: 120_000,
  retries: 0,
  use: {
    baseURL: "http://localhost:3200",
  },
  webServer: {
    command: "npx serve fixture -l 3200 --no-clipboard",
    port: 3200,
    reuseExistingServer: !process.env.CI,
  },
});
