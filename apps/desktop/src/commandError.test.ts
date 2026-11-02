// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";

import { commandErrorText } from "./commandError";
import { t } from "./i18n";
import { setLocalePreference } from "./i18n/localeStore";

afterEach(() => setLocalePreference("en"));

describe("commandErrorText", () => {
  it("keeps the store's words in English, without the code", () => {
    expect(commandErrorText("ERR_OTHER: that book is gone")).toBe(
      "that book is gone",
    );
    expect(commandErrorText(new Error("disk is full"))).toBe("disk is full");
  });

  it("never puts an English store message into Arabic", () => {
    setLocalePreference("ar");
    expect(commandErrorText("database is locked")).toBe(
      t("error.unexplained"),
    );
  });

  it("keeps text that is already Arabic", () => {
    setLocalePreference("ar");
    expect(commandErrorText("تعذّر فتح الملف")).toBe("تعذّر فتح الملف");
  });

  it("names known refusals in the reader's language", () => {
    setLocalePreference("ar");
    const job = commandErrorText("ERR_JOB_ACTIVE: an audio job is queued");
    expect(job).toBe(t("error.jobActive"));
    expect(job).toMatch(/[؀-ۿ]/);
    expect(commandErrorText("ERR_EXTRACTION_ACTIVE: stop that first")).toBe(
      t("error.extractionActive"),
    );
  });

  it("routes provider failures through the provider messages", () => {
    setLocalePreference("ar");
    expect(commandErrorText("ERR_PROVIDER_QUOTA: slow down")).toBe(
      t("job.provider.quota"),
    );
  });
});
