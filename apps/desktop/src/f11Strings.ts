// F11 session strings — catalog-backed since I18N-2 (`session.*` keys in
// messages/{en,ar}/tutor.json). Getters read the catalog at ACCESS time so
// components render the current locale; models that must not freeze English
// at import should call these inside functions, not at module top level.
import { t } from "./i18n";

export const F11_STRINGS = {
  get startLabel() {
    return t("session.start");
  },
  get pastSessions() {
    return t("session.pastSessions");
  },
  get historyLoading() {
    return t("session.historyLoading");
  },
  get historyEmptyBook() {
    return t("session.historyEmptyBook");
  },
  get historyEmptyStandalone() {
    return t("session.historyEmptyStandalone");
  },
  get historyError() {
    return t("session.historyError");
  },
  get retry() {
    return t("session.retry");
  },
  get showAll() {
    return t("session.showAll");
  },
  get continueSession() {
    return t("session.continue");
  },
  get back() {
    return t("session.back");
  },
  get open() {
    return t("session.open");
  },
  get summary() {
    return t("session.summary");
  },
  get transcript() {
    return t("session.transcript");
  },
  get you() {
    return t("tutor.you");
  },
  get tutor() {
    return t("tutor.tutor");
  },
  get reasonUnexpected() {
    return t("session.reasonUnexpected");
  },
  get reasonExpired() {
    return t("session.reasonExpired");
  },
  get reasonConnectionClosed() {
    return t("session.reasonConnectionClosed");
  },
  chapterSingle: (chapter: number): string =>
    t("session.chapterSingle", { n: chapter }),
  // One LTR island around the whole range: two bare numbers around an en dash
  // reverse visually inside an RTL run (round-2 F1).
  chapterRange: (first: number, last: number): string =>
    t("session.chapterRange", { range: `${first}–${last}` }),
  chapterList: (chapters: number[]): string =>
    t("session.chapterList", { list: chapters.join(", ") }),
  get h9Notice() {
    return t("session.h9Notice");
  },
  get liveContinue() {
    return t("session.liveContinue");
  },
  get deleteSession() {
    return t("session.deleteSession");
  },
  get deleteConfirmQuestion() {
    return t("session.deleteConfirmQuestion");
  },
  get deleteConfirmLabel() {
    return t("session.deleteConfirmLabel");
  },
  get deleteFailed() {
    return t("session.deleteFailed");
  },
  get forgetBook() {
    return t("session.forgetBook");
  },
  get forgetStandalone() {
    return t("session.forgetStandalone");
  },
  get forgetConfirmQuestion() {
    return t("session.forgetConfirmQuestion");
  },
  get forgetStandaloneConfirmQuestion() {
    return t("session.forgetStandaloneConfirmQuestion");
  },
  get forgetConfirmLabel() {
    return t("session.forgetConfirmLabel");
  },
  get forgetFailed() {
    return t("session.forgetFailed");
  },
  get keep() {
    return t("session.keep");
  },
  get today() {
    return t("session.today");
  },
  get yesterday() {
    return t("session.yesterday");
  },
  get underAMinute() {
    return t("session.underAMinute");
  },
  get transcriptLoading() {
    return t("session.transcriptLoading");
  },
  get transcriptError() {
    return t("session.transcriptError");
  },
  get historyReturnLabel() {
    return t("session.historyReturnLabel");
  },
  get retentionNever() {
    return t("session.retentionNever");
  },
  get retentionChange() {
    return t("session.retentionChange");
  },
};

export type F11StringKey = keyof typeof F11_STRINGS;
