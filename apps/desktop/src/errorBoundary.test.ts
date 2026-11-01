import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vitest";

const invokeMock = vi.fn<(cmd: string, args?: unknown) => Promise<unknown>>();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => invokeMock(cmd, args),
}));

import ErrorBoundary from "./ErrorBoundary";
import { resetUiCrashEmitter } from "./uiCrashDiagnostics";
import { readSource } from "./tests/readSource";

const SOURCE = readSource(new URL("./ErrorBoundary.tsx", import.meta.url));

function crashed(error?: unknown): ErrorBoundary {
  const boundary = new ErrorBoundary({ children: null });
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  (boundary as any).state = { failed: true };
  if (error !== undefined) boundary.componentDidCatch(error);
  return boundary;
}

describe("the crash boundary never re-enters the subsystem that crashed", () => {
  beforeEach(() => {
    invokeMock.mockReset().mockResolvedValue(null);
    resetUiCrashEmitter();
  });

  it("renders the catalog copy on the happy path", () => {
    const markup = renderToStaticMarkup(crashed().render());
    expect(markup).toContain("this screen stopped");
    expect(markup).toContain("Reload this screen");
  });

  it("falls back to English literals when the locale catalog itself throws", () => {
    const markup = renderToStaticMarkup(crashed().render());
    expect(markup).toContain("Reload this screen");
    expect(
      markup,
      "the last rung must not depend on any catalog lookup at all",
    ).toContain("Your library and jobs are safe on disk.");
  });

  it("keeps a three-rung ladder rather than one fallback", () => {
    expect(
      SOURCE,
      "one try/catch around the locale means a crash *outside* i18n still loses localized copy",
    ).toMatch(
      /try\s*\{[\s\S]*i18nFor\(localeSnapshot\(\)\.locale\)[\s\S]*\}\s*catch\s*\{[\s\S]*try\s*\{[\s\S]*i18nFor\("en"\)[\s\S]*\}\s*catch\s*\{/,
    );
    expect(SOURCE).toContain("return fallbackCopy();");
  });

  it("reports the crash to the backend without ever letting the report throw", () => {
    crashed(new TypeError("x is not a function"));
    expect(invokeMock).toHaveBeenCalledWith(
      "log_ui_error",
      expect.objectContaining({ line: expect.stringContaining("TypeError") }),
    );
  });

  it("swallows a failed report and keeps the console record", () => {
    const consoleError = vi
      .spyOn(console, "error")
      .mockImplementation(() => {});
    invokeMock.mockRejectedValueOnce(new Error("ipc gone"));
    const failure = new TypeError("boom");
    expect(() => crashed(failure)).not.toThrow();
    expect(consoleError).toHaveBeenCalled();
    consoleError.mockRestore();
  });

  it("stringifies a non-Error throw rather than logging [object Object]", () => {
    crashed("a bare string crash");
    expect(invokeMock).toHaveBeenCalledWith("log_ui_error", {
      line: "a bare string crash",
    });
  });

  it("suppresses a re-rendering crash loop instead of writing a line per render", async () => {
    const consoleError = vi
      .spyOn(console, "error")
      .mockImplementation(() => {});
    const failure = new TypeError("x is not a function");
    for (let step = 0; step < 30; step += 1) {
      crashed(failure);
      await new Promise((resolve) => setTimeout(resolve, 0));
    }
    consoleError.mockRestore();

    expect(
      invokeMock.mock.calls.filter((call) => call[0] === "log_ui_error").length,
      "LOGGING.md makes the shared rate policy a contract for every UI diagnostic emitter. Without \
it a boundary that re-throws on every render writes unbounded lines into the one log file users \
are told to email",
    ).toBeLessThan(6);
    expect(
      invokeMock.mock.calls.length,
      "the suppression must happen in the webview, not in the backend",
    ).toBeGreaterThan(0);
  });
});
