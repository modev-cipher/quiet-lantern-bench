import { describe, expect, it } from "vitest";

import { presentStatus } from "./engineStatus";

describe("presentStatus", () => {
  it("maps ready to green", () => {
    const badge = presentStatus({ state: "ready", port: 4321 });
    expect(badge.label).toBe("Engine ready");
    expect(badge.dotClass).toBe("bg-emerald-500");
  });

  it("maps starting and restarting to amber", () => {
    expect(presentStatus({ state: "starting" }).dotClass).toBe("bg-amber-500");
    const restarting = presentStatus({ state: "restarting", attempt: 3 });
    expect(restarting.dotClass).toBe("bg-amber-500");
    expect(restarting.label).toContain("attempt 3");
  });

  it("maps down and failed to red", () => {
    expect(presentStatus({ state: "down" }).dotClass).toBe("bg-red-500");
    const failed = presentStatus({
      state: "failed",
      errorCode: "CRASH",
      message: "Process terminated",
    });
    expect(failed.dotClass).toBe("bg-red-500");
    expect(failed.label).toContain("Process terminated");
  });

  it("maps downloading and extracting to amber with percentage", () => {
    const downloading = presentStatus({
      state: "downloading",
      bytesDone: 50,
      bytesTotal: 100,
      speedBps: 1024,
    });
    expect(downloading.dotClass).toBe("bg-amber-500");
    expect(downloading.label).toBe("Downloading engine (50%)…");

    const extracting = presentStatus({
      state: "extracting",
      filesDone: 8,
      filesTotal: 10,
    });
    expect(extracting.dotClass).toBe("bg-amber-500");
    expect(extracting.label).toBe("Extracting engine (80%)…");
  });

  it("maps notInstalled and null to neutral", () => {
    const notInstalled = presentStatus({ state: "notInstalled" });
    expect(notInstalled.label).toBe("Engine not installed");
    expect(notInstalled.dotClass).toBe("bg-neutral-500");

    const badge = presentStatus(null);
    expect(badge.label).toBe("Engine status unknown");
    expect(badge.dotClass).toBe("bg-neutral-500");
  });
});

describe("an engine from an older app version", () => {
  it("reads as needing an update, not as failed or missing", () => {
    expect(
      presentStatus({ state: "needsUpdate", installedApi: null, requiredApi: 1 }),
    ).toEqual({ label: "Engine needs an update", dotClass: "bg-amber-500" });
  });
});
