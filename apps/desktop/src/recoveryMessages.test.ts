import { expect, it } from "vitest";
import { i18nFor, type MessageDescriptor } from "./i18n";
import { stripIsolates } from "./i18n/format";
import { doneMessage, plainError } from "./lanSendModel";
import {
  uncheckedNotice,
  scoredNothingNotice,
  staleExportNotice,
  werLabel,
  type VerifyBlock,
} from "./verifyModel";

function arabic(message: MessageDescriptor | null) {
  expect(message).not.toBeNull();
  return stripIsolates(i18nFor("ar").t(message!.key, message!.params));
}

it.each([0, 1, 2, 3, 11, 102])(
  "renders the Arabic plural category for %i transferred books",
  (count) => {
    const text = arabic(doneMessage(count));
    expect(text).toMatch(/كتاب|كتب/);
    expect(text).not.toMatch(/audiobook|Sent/);
    if (count >= 3) expect(text).toContain(String(count));
  },
);

it("retains the distinction between unchecked and checker failure, including counts", () => {
  const block: VerifyBlock = {
    enabled: true,
    threshold: 0.15,
    counts: {
      ok: 0,
      failed: 0,
      missing: 0,
      unchecked: 469,
      unscoreable_ref: 0,
    },
    worst: [],
  };
  expect(arabic(uncheckedNotice(block))).toContain("لم يعمل فحص الدقة على 469");
  expect(scoredNothingNotice(block)).toBeNull();
  block.counts.unchecked = 0;
  block.counts.missing = 469;
  expect(uncheckedNotice(block)).toBeNull();
  expect(arabic(scoredNothingNotice(block))).toContain("وليس ناجحًا");
  expect(arabic(staleExportNotice(3))).toContain("3 ملفات");
  expect(arabic(werLabel(1.42))).toBe("142% اختلاف");
  expect(arabic(plainError("could not open a port: EACCES"))).toContain(
    "قد يمنعه برنامج آخر",
  );
  expect(arabic(plainError("unexpected detail"))).toContain(
    "unexpected detail",
  );
});
