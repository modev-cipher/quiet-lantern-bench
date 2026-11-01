import { createElement, Fragment } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import {
  formatMessage,
  formatNodes,
  selectForm,
  type PluralForms,
  stripIsolates,
} from "./format";

const PAGES: PluralForms = {
  zero: "لا صفحات",
  one: "صفحة واحدة",
  two: "صفحتان",
  few: "{count|n} صفحات",
  many: "{count|n} صفحة",
  other: "{count|n} صفحة",
};

describe("message format", () => {
  it("interpolates plain params; numbers become LTR isolates", () => {
    expect(
      formatMessage("en", "Page {pageNo|n} of {total|n}", {
        pageNo: 3,
        total: 1200,
      }),
    ).toBe("Page \u20663\u2069 of \u20661,200\u2069");
  });

  it("plain strings (aria-label, title) isolate every island: LRI…PDI for ltr/n, FSI…PDI for bdi", () => {
    // Kritias round 1 MUST-FIX: an Arabic aria-label carrying a path, `API` and a number.
    const s = formatMessage(
      "ar",
      "مفتاح {name|ltr} في {path|ltr} · {n|n} · {title|bdi}",
      {
        name: "API",
        path: "/mock/data/models",
        n: 2048,
        title: "Fixture Book",
      },
    );
    expect(s).toBe(
      "مفتاح \u2066API\u2069 في \u2066/mock/data/models\u2069 · \u20662,048\u2069 · \u2068Fixture Book\u2069",
    );
    // every isolate opened is closed, and nothing else is a control character
    expect((s.match(/[\u2066\u2068]/g) ?? []).length).toBe(4);
    expect((s.match(/\u2069/g) ?? []).length).toBe(4);
    expect(stripIsolates(s)).toBe(
      "مفتاح API في /mock/data/models · 2,048 · Fixture Book",
    );
  });

  it("React output isolates with <bdi> only — no control characters leak into text nodes (no double isolation)", () => {
    const nodes = formatNodes("ar", "المسار {path|ltr} · {n|n} · {title|bdi}", {
      path: "/mock/data/models",
      n: 2048,
      title: "Fixture Book",
    });
    const html = renderToStaticMarkup(createElement(Fragment, null, ...nodes));
    expect(html).not.toMatch(/[\u2066-\u2069]/);
    expect(html).toContain(
      '<bdi dir="ltr" class="ltr-island">/mock/data/models</bdi>',
    );
    expect(html).toContain("<bdi>Fixture Book</bdi>");
  });

  it("React output renders islands as <bdi dir=ltr class=ltr-island>", () => {
    const html = renderToStaticMarkup(
      createElement(
        Fragment,
        null,
        ...formatNodes("ar", "المسار {path|ltr} · {n|n} Token", {
          path: "/mock/data/models",
          n: 2048,
        }),
      ),
    );
    expect(html).toBe(
      'المسار <bdi dir="ltr" class="ltr-island">/mock/data/models</bdi> · <bdi dir="ltr" class="ltr-island">2,048</bdi> Token',
    );
  });

  it("picks the Arabic plural form for each of the six categories", () => {
    const forms = [0, 1, 2, 3, 11, 100].map((count) =>
      formatMessage("ar", PAGES, { count }),
    );
    expect(forms).toEqual([
      "لا صفحات",
      "صفحة واحدة",
      "صفحتان",
      "\u20663\u2069 صفحات",
      "\u206611\u2069 صفحة",
      "\u2066100\u2069 صفحة",
    ]);
  });

  it("English plural falls back to `other` when a category is not written", () => {
    expect(
      selectForm(
        "en",
        { one: "{count} page", other: "{count} pages" },
        { count: 5 },
      ),
    ).toBe("{count} pages");
    expect(selectForm("en", { other: "pages" }, { count: 1 })).toBe("pages");
  });

  it("a missing param is left visible, never thrown", () => {
    expect(formatMessage("en", "Hello {name}")).toBe("Hello {name}");
  });
});
