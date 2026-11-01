// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";

const invokeMock = vi.fn<(command: string) => Promise<unknown>>();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (command: string) => invokeMock(command),
}));
afterEach(() => {
  localStorage.clear();
  vi.resetModules();
  invokeMock.mockReset();
});

it("a late startup preference cannot replace an explicit first-run language choice", async () => {
  let finishHost: (value: string) => void = () => {};
  let finishPreference: (value: string) => void = () => {};
  invokeMock.mockImplementation((command) =>
    command === "host_locale"
      ? new Promise((done) => {
          finishHost = done;
        })
      : command === "get_locale_preference"
        ? new Promise((done) => {
            finishPreference = done;
          })
        : Promise.resolve(null),
  );
  const store = await import("./localeStore");
  store.initLocale();
  store.setLocalePreference("ar");
  finishPreference("en");
  finishHost("en_US");
  await vi.waitFor(() => expect(store.getSnapshot().hostLocale).toBe("en_US"));
  expect(store.getSnapshot().preference).toBe("ar");
  expect(document.documentElement.lang).toBe("ar");
});
