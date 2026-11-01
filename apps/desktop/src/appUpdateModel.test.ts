import { describe, expect, it } from "vitest";

import {
  bannerFor,
  formatCheckedAt,
  percentOf,
  statusLineFor,
  type UpdateSnapshot,
} from "./appUpdateModel";
import { numberLocale } from "./i18n/localeModel";

const base: UpdateSnapshot = {
  phase: "idle",
  enabled: true,
  current_version: "0.1.0",
  available_version: null,
  notes: null,
  release_url: null,
  install_kind: null,
  received: 0,
  total: 0,
  error_code: null,
  error_message: null,
  last_checked_unix: null,
  last_attempt_offline: false,
  dismissed: false,
};

const ready: UpdateSnapshot = {
  ...base,
  phase: "available",
  available_version: "0.2.0",
  notes: "Faster casts",
  install_kind: "restart",
  last_checked_unix: 1_757_000_000,
};

describe("bannerFor", () => {
  it("shows nothing for idle, checking, up to date, paused, disabled, or null", () => {
    for (const phase of ["idle", "checking", "up_to_date", "paused"] as const) {
      expect(bannerFor({ ...base, phase })).toBeNull();
    }
    expect(bannerFor({ ...ready, enabled: false })).toBeNull();
    expect(bannerFor(null)).toBeNull();
  });

  it("offers the version with its notes and the right action", () => {
    expect(bannerFor(ready)).toEqual({
      kind: "available",
      version: "0.2.0",
      notes: "Faster casts",
      action: "restart",
    });
    expect(bannerFor({ ...ready, install_kind: "open" })).toMatchObject({
      action: "open",
    });
  });

  it("Later hides the offer", () => {
    expect(bannerFor({ ...ready, dismissed: true })).toBeNull();
  });

  it("reports progress as a floored, clamped percentage", () => {
    expect(
      bannerFor({ ...ready, phase: "downloading", received: 333, total: 1000 }),
    ).toEqual({ kind: "progress", phase: "downloading", percent: 33 });
    expect(
      bannerFor({ ...ready, phase: "downloading", received: 5, total: 0 }),
    ).toMatchObject({ percent: null });
    expect(bannerFor({ ...ready, phase: "verifying" })).toEqual({
      kind: "progress",
      phase: "verifying",
      percent: null,
    });
    expect(bannerFor({ ...ready, phase: "installing" })).toMatchObject({
      phase: "installing",
    });
  });

  it("a failure names the version that is still running", () => {
    expect(
      bannerFor({ ...ready, phase: "failed", error_code: "digest" }),
    ).toEqual({ kind: "failed", currentVersion: "0.1.0" });
  });
});

describe("percentOf", () => {
  it("clamps and floors", () => {
    expect(percentOf(0, 10)).toBe(0);
    expect(percentOf(999, 1000)).toBe(99);
    expect(percentOf(20, 10)).toBe(100);
    expect(percentOf(1, -1)).toBeNull();
  });
});

describe("statusLineFor", () => {
  it("disabled builds and missing snapshots are unavailable", () => {
    expect(statusLineFor(null)).toEqual({ kind: "unavailable" });
    expect(statusLineFor({ ...base, enabled: false })).toEqual({
      kind: "unavailable",
    });
  });

  it("never checked vs offline before any check", () => {
    expect(statusLineFor(base)).toEqual({ kind: "never" });
    expect(statusLineFor({ ...base, last_attempt_offline: true })).toEqual({
      kind: "offline",
    });
    expect(statusLineFor({ ...base, phase: "checking" })).toEqual({
      kind: "checking",
    });
  });

  it("checked: up to date, available, offline-since, or paused (time only)", () => {
    const at = 1_757_000_000;
    expect(
      statusLineFor({ ...base, phase: "up_to_date", last_checked_unix: at }),
    ).toEqual({ kind: "checked", at, detail: "up_to_date", version: null });
    expect(statusLineFor(ready)).toEqual({
      kind: "checked",
      at,
      detail: "available",
      version: "0.2.0",
    });
    expect(
      statusLineFor({ ...ready, last_attempt_offline: true }),
    ).toMatchObject({ detail: "offline" });
    expect(
      statusLineFor({ ...base, phase: "paused", last_checked_unix: at }),
    ).toEqual({ kind: "checked", at, detail: null, version: null });
  });
});

describe("formatCheckedAt", () => {
  it("uses time only for today and adds the date otherwise, in the locale", () => {
    const now = Date.UTC(2026, 8, 7, 12, 0, 0);
    const today = formatCheckedAt(now / 1000 - 3600, "en-US", now);
    expect(today).toMatch(/\d{1,2}:\d{2}/);
    expect(today).not.toMatch(/Sep/);
    const lastWeek = formatCheckedAt(now / 1000 - 7 * 86400, "en-US", now);
    expect(lastWeek).toMatch(/Aug|Sep/);
    const arabic = formatCheckedAt(now / 1000 - 7 * 86400, numberLocale("ar"), now);
    expect(arabic).not.toMatch(/[٠-٩]/u);
  });
});
