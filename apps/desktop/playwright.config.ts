import { defineConfig } from "@playwright/test";

import { readingProjects } from "./src/playwrightProjects";

const projects = readingProjects(process.env);

export default defineConfig({
  testDir: "./e2e",
  fullyParallel: false,
  workers: 1,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? "line" : "list",
  use: {
    baseURL: "http://localhost:1421",
    channel: process.env.PW_CHANNEL ?? "chromium",
    trace: "retain-on-failure",
  },
  ...(projects.length > 0 ? { projects } : {}),
  webServer: {
    command: "pnpm exec vite --port 1421 --strictPort",
    url: "http://localhost:1421",
    reuseExistingServer: !process.env.CI,
  },
});
