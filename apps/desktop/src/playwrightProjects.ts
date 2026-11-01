export interface PlaywrightProject {
  name: string;
  use: { channel?: string; browserName?: "chromium" | "webkit" };
}

export const WEBKIT_ENV = "PLAYWRIGHT_WEBKIT";

export function readingProjects(
  env: Record<string, string | undefined>,
): PlaywrightProject[] {
  if (env[WEBKIT_ENV] !== "1") return [];
  return [
    { name: "chromium", use: { channel: "chromium" } },
    { name: "webkit", use: { browserName: "webkit" } },
  ];
}
