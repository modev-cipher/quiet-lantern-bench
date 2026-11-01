import { useI18n } from "./i18n";
import { useRef } from "react";

export default function TutorNavigationControls({
  pdf,
  held,
  latestPage,
  shownPage,
  onKeep,
  onReturn,
}: {
  pdf: boolean;
  held: boolean;
  latestPage: number | null;
  shownPage: number | null;
  onKeep: () => void;
  onReturn: () => void;
}) {
  const { t } = useI18n();
  const mainButton = useRef<HTMLButtonElement>(null);
  return (
    <div
      data-testid="tutor-navigation-controls"
      className="flex min-w-0 flex-wrap items-center gap-3 text-label"
    >
      <span role="status">
        {held ? t("reader.tutorHeld") : t("reader.tutorGuides")}
      </span>
      {shownPage != null && (
        <span>
          {t(pdf ? "reader.pdfPage" : "reader.documentPage", {
            page: shownPage,
          })}
        </span>
      )}
      {held && latestPage != null && latestPage !== shownPage && (
        <span>
          {t(
            pdf ? "reader.tutorDestination" : "reader.tutorDocumentDestination",
            { page: latestPage },
          )}
        </span>
      )}
      <button
        ref={mainButton}
        type="button"
        data-testid={held ? "reading-return" : "tutor-keep-page"}
        onClick={held ? onReturn : onKeep}
        className={`rounded-full border px-3 py-1.5 ${held ? "border-ember-500/60 bg-ember-500/15 text-ember-200" : "border-ink-700 hover:bg-ink-800"}`}
      >
        {held ? t("reader.goToTutor") : t("reader.keepMyPage")}
      </button>
      {!held && latestPage != null && latestPage !== shownPage && (
        <button
          type="button"
          data-testid="reading-return"
          onClick={() => {
            onReturn();
            mainButton.current?.focus();
          }}
          className="rounded-full border border-ember-500/60 bg-ember-500/15 px-3 py-1.5 text-ember-200"
        >
          {t("reader.goToTutor")}
        </button>
      )}
    </div>
  );
}
