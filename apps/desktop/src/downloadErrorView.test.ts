import { describe, expect, it } from "vitest";

import { downloadErrorView } from "./downloadErrorView";
import { t } from "./i18n";

describe("downloadErrorView", () => {
  it("turns a dropped connection into a calm, resumable message and keeps the raw text as detail", () => {
    const raw = "network error: error decoding response body";
    expect(downloadErrorView(raw)).toEqual({
      kind: "connection",
      message: t("models.error.connection"),
      detail: raw,
      resumable: true,
    });
  });

  it("treats a stream that ended early and a server hiccup as connection problems too", () => {
    expect(
      downloadErrorView(
        "size mismatch for 'model.gguf': expected 524288 bytes, got 102400",
      )?.kind,
    ).toBe("connection");
    expect(
      downloadErrorView("server returned status 503 for 'https://x'")?.kind,
    ).toBe("connection");
  });

  it("names disk space, a bad checksum and a missing file plainly", () => {
    expect(
      downloadErrorView(
        "insufficient disk space: needs 2147483648 bytes, 100 bytes free",
      )?.kind,
    ).toBe("disk");
    expect(
      downloadErrorView("hash mismatch for 'model.gguf': expected aa, got bb"),
    ).toMatchObject({ kind: "corrupt", resumable: false });
    expect(
      downloadErrorView("server returned status 404 for 'https://x'")?.kind,
    ).toBe("unavailable");
  });

  it("says nothing for a cancel the user asked for", () => {
    expect(downloadErrorView("download cancelled")).toBeNull();
  });

  it("falls back to a generic message without losing the original", () => {
    expect(downloadErrorView("io error: something odd")).toEqual({
      kind: "other",
      message: t("models.error.generic"),
      detail: "io error: something odd",
      resumable: true,
    });
  });
});
