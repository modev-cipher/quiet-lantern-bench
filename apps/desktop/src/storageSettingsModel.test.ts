import { describe, expect, it } from "vitest";

import {
  BYTES_PER_GB,
  cacheUsageOf,
  capGbInputFor,
  capNoticeOf,
  capRequest,
  dirStatusOf,
  formatBytes,
  formatCapBytes,
  parseCapGb,
  pathTail,
  type StorageBreakdown,
  type StorageDirView,
  type StorageSettings,
} from "./storageSettingsModel";

function dir(overrides: Partial<StorageDirView> = {}): StorageDirView {
  return {
    path: "/home/u/.local/share/io.github.meedoomostafa.txtcaster/models",
    defaultPath:
      "/home/u/.local/share/io.github.meedoomostafa.txtcaster/models",
    isDefault: true,
    configured: null,
    rejected: null,
    ...overrides,
  };
}

function settings(overrides: Partial<StorageSettings> = {}): StorageSettings {
  return {
    models: dir(),
    exports: dir({ path: "/exports", defaultPath: "/exports" }),
    cacheDir: "/cache",
    cacheBytes: 3 * BYTES_PER_GB,
    cacheCapBytes: 20 * BYTES_PER_GB,
    cacheCapSource: "default",
    cacheCapRefused: null,
    defaultCacheCapBytes: 20 * BYTES_PER_GB,
    ...overrides,
  };
}

describe("capNoticeOf", () => {
  it("names a refused stored cap so the default in force is explained", () => {
    const refused = settings({
      cacheCapSource: "default",
      cacheCapRefused: "20 GB",
      cacheCapBytes: 20 * BYTES_PER_GB,
    });
    expect(capNoticeOf(refused)).toContain("20 GB");
  });

  it("has no notice when the stored cap was usable", () => {
    expect(capNoticeOf(settings())).toBeNull();
  });
});

describe("cacheUsageOf", () => {
  it("states measured usage against the cap in force", () => {
    expect(cacheUsageOf(settings())).toBe(
      "\u20663\u2069 GB of \u206620\u2069 GB used",
    );
  });

  it("states usage without a cap when there is no limit", () => {
    expect(
      cacheUsageOf(
        settings({ cacheCapSource: "unlimited", cacheCapBytes: null }),
      ),
    ).toBe("\u20663\u2069 GB used, no limit");
  });
});

describe("capRequest", () => {
  it("clears the setting for the default so the app keeps following the shipped cap", () => {
    expect(capRequest("default", 7)).toBeNull();
  });

  it("sends zero for no limit, which is a stored choice rather than an absent row", () => {
    expect(capRequest("unlimited", 7)).toBe(0);
  });

  it("converts GB to whole bytes", () => {
    expect(capRequest("custom", 7)).toBe(7 * BYTES_PER_GB);
    expect(capRequest("custom", 0.5)).toBe(BYTES_PER_GB / 2);
  });

  it("never emits a fractional byte count", () => {
    const bytes = capRequest("custom", 1 / 3);
    expect(Number.isInteger(bytes)).toBe(true);
  });
});

describe("parseCapGb", () => {
  it("accepts plain and fractional sizes", () => {
    expect(parseCapGb("20")).toEqual({ gb: 20 });
    expect(parseCapGb("  7.5 ")).toEqual({ gb: 7.5 });
  });

  it("refuses units, blanks and non-numbers with a reason", () => {
    for (const raw of ["", "   ", "20 GB", "twenty", "1e9", "-1", "0x10"]) {
      const result = parseCapGb(raw);
      expect("error" in result, `raw ${JSON.stringify(raw)}`).toBe(true);
    }
  });

  it("sends a zero size to the No limit option instead of storing an unreachable cap", () => {
    const result = parseCapGb("0");
    expect(result).toEqual({ error: "A cap of 0 GB is the No limit option." });
  });

  it("refuses a size that cannot be represented exactly", () => {
    const result = parseCapGb("99999999999");
    expect("error" in result).toBe(true);
  });
});

describe("formatCapBytes", () => {
  it("names the unlimited case rather than printing a zero", () => {
    expect(formatCapBytes(null)).toBe("No limit");
  });

  it("prints whole GB without a decimal point and part GB with one", () => {
    expect(formatCapBytes(20 * BYTES_PER_GB)).toBe("\u206620\u2069 GB");
    expect(formatCapBytes(BYTES_PER_GB / 2)).toBe("\u20660.5\u2069 GB");
  });
});

describe("capGbInputFor", () => {
  it("seeds the field from the default when nothing is stored", () => {
    expect(capGbInputFor(settings())).toBe("20");
  });

  it("seeds the field from the default when the stored choice is no limit", () => {
    expect(
      capGbInputFor(
        settings({ cacheCapSource: "unlimited", cacheCapBytes: null }),
      ),
    ).toBe("20");
  });

  it("seeds the field from the stored cap when there is one", () => {
    expect(
      capGbInputFor(
        settings({
          cacheCapSource: "custom",
          cacheCapBytes: 5 * BYTES_PER_GB,
        }),
      ),
    ).toBe("5");
  });
});

describe("dirStatusOf", () => {
  it("says default when nothing is configured", () => {
    expect(dirStatusOf(dir())).toBe("Default location");
  });

  it("says custom when a configured path is in force", () => {
    expect(
      dirStatusOf(dir({ isDefault: false, configured: "/mnt/d/models" })),
    ).toBe("Custom location");
  });

  it("surfaces a refused configured path and its reason instead of silently defaulting", () => {
    const status = dirStatusOf(
      dir({
        configured: "models/tts",
        rejected: "the path is not absolute",
      }),
    );
    expect(status).toContain("models/tts");
    expect(status).toContain("the path is not absolute");
  });
});

describe("a path row shows the part that tells the rows apart", () => {
  it("keeps the tail, because three rows under one profile differ only at the end", () => {
    const root = "/home/medo/.local/share/io.github.meedoomostafa.txtcaster";
    expect(pathTail(`${root}/models`)).toBe(
      "…/io.github.meedoomostafa.txtcaster/models",
    );
    expect(pathTail(`${root}/exports`)).toBe(
      "…/io.github.meedoomostafa.txtcaster/exports",
    );
    expect(pathTail(`${root}/cache`)).toBe(
      "…/io.github.meedoomostafa.txtcaster/cache",
    );
  });

  it("gives three different strings for three different directories", () => {
    const root = "/home/medo/.local/share/io.github.meedoomostafa.txtcaster";
    const shown = ["models", "exports", "cache"].map((leaf) =>
      pathTail(`${root}/${leaf}`),
    );
    expect(new Set(shown).size, "head truncation collapsed all three").toBe(3);
  });

  it("leaves a short path whole", () => {
    expect(pathTail("/models")).toBe("/models");
    expect(pathTail("")).toBe("");
  });

  it("keeps a windows path on its own separator", () => {
    expect(pathTail("C:\\Users\\medo\\txtcaster\\models")).toBe(
      "…\\txtcaster\\models",
    );
  });
});

describe("formatBytes re-export", () => {
  it("formats sizes in GB, MB, KB", () => {
    expect(formatBytes(2_500_000_000)).toBe("2.5 GB");
    expect(formatBytes(350_000_000)).toBe("350 MB");
    expect(formatBytes(50_000)).toBe("50 KB");
  });
});

describe("StorageBreakdown structure", () => {
  it("accepts a fully populated breakdown shape", () => {
    const breakdown: StorageBreakdown = {
      totalBytes: 5_000_000_000,
      engine: {
        status: "installed",
        diskBytes: 1_200_000_000,
        path: "/home/user/.local/share/txtcaster/engine/txtcaster-engine",
      },
      models: [
        {
          modelId: "kokoro-v1",
          name: "Kokoro v1",
          task: "tts",
          diskBytes: 300_000_000,
          installedAt: 1710000000,
        },
      ],
      cache: {
        diskBytes: 500_000_000,
        activeJobLocks: 0,
      },
      books: {
        count: 12,
        diskBytes: 2_000_000_000,
      },
      exports: {
        count: 5,
        diskBytes: 1_000_000_000,
      },
    };
    expect(breakdown.engine.status).toBe("installed");
    expect(breakdown.models).toHaveLength(1);
    expect(breakdown.cache.activeJobLocks).toBe(0);
  });
});
