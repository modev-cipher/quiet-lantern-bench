import { describe, expect, it, vi } from "vitest";

vi.mock("./i18n", () => ({ t: (key: string) => key }));

import {
  engineRefusal,
  engineSatisfies,
  fullVariantBody,
  installErrorKey,
  installOptions,
  progressFraction,
  type EngineInstallInfo,
} from "./engineSetupModel";

const ready = { state: "ready", port: 1 } as const;

function info(variants: string[]): EngineInstallInfo {
  return {
    installed: null,
    requiredApi: 1,
    platform: "linux-x86_64",
    offered: variants.map((variant) => ({
      variant,
      downloadBytes: 1,
      installedBytes: 2,
      spaceNeededBytes: 3,
    })),
    engineVersion: "1.0.0",
    recommended: { variant: "cpu", reason: "no_nvidia_gpu", gpuName: null },
    spaceAvailableBytes: 10,
    offerError: null,
    installing: false,
    liteAvailable: variants.includes("lite"),
  };
}

describe("engineSatisfies", () => {
  it("asks for setup when no usable engine exists", () => {
    expect(engineSatisfies({ state: "notInstalled" }, null, "documents")).toBe(
      false,
    );
    expect(
      engineSatisfies(
        { state: "needsUpdate", installedApi: 0, requiredApi: 1 },
        null,
        "documents",
      ),
    ).toBe(false);
  });

  it("lets the reader engine read books but not run local models", () => {
    expect(engineSatisfies(ready, "lite", "documents")).toBe(true);
    expect(engineSatisfies(ready, "lite", "localAi")).toBe(false);
    expect(engineSatisfies(ready, null, "localAi")).toBe(true);
  });

  it("does not interrupt an engine that is still starting", () => {
    expect(engineSatisfies({ state: "starting" }, null, "localAi")).toBe(true);
  });
});

describe("engineRefusal", () => {
  it("recognises native gates only", () => {
    expect(engineRefusal("ERR_ENGINE_NOT_INSTALLED:documents")).toBe(
      "documents",
    );
    expect(
      engineRefusal("invalid request: ERR_ENGINE_LOCAL_AI_REQUIRED:localAi"),
    ).toBe("localAi");
    expect(engineRefusal("ERR_FFMPEG_MISSING: no encoder")).toBeNull();
  });
});

describe("installOptions", () => {
  it("offers the reader engine first for reading and only the full build for local models", () => {
    const all = info(["cpu", "lite", "cuda13"]);
    expect(
      installOptions(all, "documents", "cpu").map((o) => o.variant),
    ).toEqual(["lite", "cpu"]);
    expect(
      installOptions(all, "localAi", "cuda13").map((o) => o.variant),
    ).toEqual(["cuda13"]);
  });

  it("does not invent a variant the release lacks", () => {
    expect(
      installOptions(info(["cpu"]), "documents", "cpu").map((o) => o.variant),
    ).toEqual(["cpu"]);
    expect(installOptions(info([]), "documents", "cpu")).toEqual([]);
  });
});

describe("explanations", () => {
  it("names why a build was chosen", () => {
    const choice = (reason: EngineInstallInfo["recommended"]["reason"]) => ({
      variant: reason === "nvidia_ready" ? "cuda13" : "cpu",
      reason,
      gpuName: "GPU",
    });
    expect(fullVariantBody(choice("nvidia_ready"), "cuda13")).toBe(
      "engine.setup.fullGpu",
    );
    expect(fullVariantBody(choice("driver_too_old"), "cpu")).toBe(
      "engine.setup.fullCpuOldDriver",
    );
    expect(fullVariantBody(choice("gpu_too_old"), "cpu")).toBe(
      "engine.setup.fullCpuOldGpu",
    );
    expect(fullVariantBody(choice("nvidia_ready"), "cpu")).toBe(
      "engine.setup.fullCpu",
    );
  });

  it("maps install failures to recovery copy", () => {
    expect(installErrorKey("ERR_ENGINE_OFFLINE: dns")).toBe(
      "engine.setup.error.offline",
    );
    expect(installErrorKey("ERR_ENGINE_VERIFY")).toBe(
      "engine.setup.error.verify",
    );
    expect(installErrorKey("ERR_JOB_ACTIVE: running")).toBe(
      "engine.setup.error.jobs",
    );
    expect(installErrorKey("surprise")).toBe("engine.setup.error.generic");
  });

  it("reports a fraction only while bytes are moving", () => {
    expect(
      progressFraction({
        phase: "downloading",
        bytesDone: 1,
        bytesTotal: 4,
        speedBps: 0,
      }),
    ).toBe(0.25);
    expect(progressFraction({ phase: "verifying" })).toBeNull();
  });
});
