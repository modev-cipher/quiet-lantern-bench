import { Fragment, useEffect, useId, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { openUrl } from "@tauri-apps/plugin-opener";

import {
  BugIcon,
  ChevronIcon,
  CloseIcon,
  FileTextIcon,
  GlobeIcon,
  HeartIcon,
  MailIcon,
  ShieldIcon,
  SparkIcon,
} from "./Icons";
import { useDialogFocus } from "./useDialogFocus";
import { useI18n } from "./i18n";
import {
  REPORT_ISSUE_OPTIONS,
  type ReportIssueOption,
} from "./reportIssueModel";

export interface ReportIssueModalProps {
  open: boolean;
  onClose: () => void;
  version?: string;
  onOpenUrl?: (url: string) => Promise<void> | void;
}

const OPTION_ICONS: Record<
  ReportIssueOption["id"],
  (props: { className?: string }) => React.JSX.Element
> = {
  bug: BugIcon,
  feature: SparkIcon,
  email: MailIcon,
  blank: FileTextIcon,
  website: GlobeIcon,
  security: ShieldIcon,
  support: HeartIcon,
};

export default function ReportIssueModal({
  open,
  onClose,
  onOpenUrl,
}: ReportIssueModalProps) {
  const dialogRef = useRef<HTMLDivElement>(null);
  const linkRef = useRef<HTMLInputElement>(null);
  const errorRef = useRef<HTMLParagraphElement>(null);
  const operation = useRef(0);
  const busy = useRef(false);
  const [pending, setPending] = useState<"open" | "copy" | null>(null);
  const [failedOption, setFailedOption] = useState<ReportIssueOption | null>(
    null,
  );
  const [copyState, setCopyState] = useState<"idle" | "copied" | "failed">(
    "idle",
  );
  const linkId = useId();
  const { t } = useI18n();

  useEffect(() => {
    busy.current = false;
    setPending(null);
    setFailedOption(null);
    setCopyState("idle");
    return () => {
      operation.current += 1;
    };
  }, [open]);

  useEffect(() => {
    if (failedOption) errorRef.current?.focus();
  }, [failedOption]);

  const dismiss = () => {
    operation.current += 1;
    busy.current = false;
    setPending(null);
    onClose();
  };

  useDialogFocus(dialogRef, open, { onEscape: dismiss });

  const handleSelect = async (option: ReportIssueOption) => {
    if (busy.current) return;
    busy.current = true;
    const request = ++operation.current;
    setPending("open");
    setCopyState("idle");
    try {
      if (onOpenUrl) {
        await onOpenUrl(option.url);
      } else {
        await openUrl(option.url);
      }
      if (request === operation.current) dismiss();
    } catch {
      if (request !== operation.current) return;
      busy.current = false;
      setPending(null);
      setFailedOption({ ...option });
    }
  };

  const copyLink = async () => {
    if (!failedOption || busy.current) return;
    busy.current = true;
    const request = ++operation.current;
    const source = document.activeElement;
    setPending("copy");
    setCopyState("idle");
    let copied = false;
    try {
      if (navigator.clipboard?.writeText) {
        await navigator.clipboard.writeText(failedOption.url);
        copied = true;
      }
    } catch {
      copied = false;
    }
    if (request !== operation.current) return;
    if (!copied) {
      linkRef.current?.focus();
      linkRef.current?.select();
      try {
        copied = document.execCommand?.("copy") === true;
      } catch {
        copied = false;
      }
      if (copied && source instanceof HTMLElement && source.isConnected)
        source.focus();
    }
    busy.current = false;
    setPending(null);
    setCopyState(copied ? "copied" : "failed");
  };

  if (!open) return null;

  const status =
    pending === "open"
      ? t("issue.opening")
      : pending === "copy"
        ? t("issue.copying")
        : copyState === "copied"
          ? t("issue.linkCopied")
          : null;

  const content = (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center p-4"
      data-testid="report-issue-modal"
      data-modal-layer=""
    >
      <button
        type="button"
        aria-label={t("issue.close")}
        onClick={dismiss}
        className="animate-fade-in absolute inset-0 cursor-default bg-ink-950/85 backdrop-blur-sm"
      />
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-label={t("issue.modalTitle")}
        tabIndex={-1}
        className="animate-modal-in relative flex max-h-[calc(100dvh-2rem)] w-full max-w-lg flex-col overflow-hidden rounded-2xl border border-ink-800 bg-ink-900 shadow-overlay outline-none"
      >
        <div className="flex shrink-0 items-start justify-between gap-4 p-4 sm:p-6">
          <div className="min-w-0">
            <h2 className="font-serif text-title font-semibold tracking-tight text-ink-50">
              {t("issue.modalTitle")}
            </h2>
            <p className="mt-1 text-secondary text-ink-400">
              {t("issue.modalSubtitle")}
            </p>
          </div>
          <button
            type="button"
            aria-label={t("issue.close")}
            data-testid="report-issue-close"
            onClick={dismiss}
            className="shrink-0 rounded-lg p-1 text-ink-500 transition-colors hover:bg-ink-850 hover:text-ink-200"
          >
            <CloseIcon className="h-5 w-5" />
          </button>
        </div>

        <div
          data-testid="report-issue-body"
          className="flex min-h-0 flex-col gap-2.5 overflow-y-auto overscroll-contain px-4 pb-4 sm:px-6 sm:pb-6"
        >
          {failedOption && (
            <div className="flex flex-col gap-3 rounded-xl border border-wash-red-ink/30 bg-wash-red p-3.5 text-wash-red-ink">
              <p
                ref={errorRef}
                role="alert"
                tabIndex={-1}
                className="text-secondary outline-none"
              >
                {t("issue.openFailed")}
              </p>
              <label htmlFor={linkId} className="text-label font-medium">
                {t("issue.link")}
              </label>
              {copyState === "failed" && (
                <p role="alert" className="text-secondary">
                  {t("issue.copyFailed")}
                </p>
              )}
              <input
                ref={linkRef}
                id={linkId}
                dir="ltr"
                readOnly
                value={failedOption.url}
                onFocus={(event) => event.currentTarget.select()}
                className="w-full min-w-0 rounded-lg border border-ink-700 bg-ink-950 px-3 py-2 font-mono text-label text-ink-100 outline-none focus:border-ember-500"
              />
              <div className="flex flex-wrap gap-2">
                <button
                  type="button"
                  aria-disabled={pending != null}
                  onClick={() => void handleSelect(failedOption)}
                  className="rounded-lg bg-ink-50 px-3 py-2 text-label font-semibold text-ink-950 aria-disabled:opacity-50"
                >
                  {t("issue.retry")}
                </button>
                <button
                  type="button"
                  data-testid="issue-copy-link"
                  aria-disabled={pending != null}
                  onClick={() => void copyLink()}
                  className="rounded-lg border border-ink-700 bg-ink-900 px-3 py-2 text-label text-ink-100 aria-disabled:opacity-50"
                >
                  {copyState === "copied"
                    ? t("issue.linkCopied")
                    : t("issue.copyLink")}
                </button>
              </div>
            </div>
          )}
          {status && (
            <p role="status" className="text-secondary text-ink-300">
              {status}
            </p>
          )}
          {REPORT_ISSUE_OPTIONS.map((option) => {
            const Icon = OPTION_ICONS[option.id];
            return (
              <Fragment key={option.id}>
                {option.id === "support" && (
                  <hr className="my-1 shrink-0 border-ink-800" />
                )}
                <button
                  type="button"
                  data-testid={`issue-option-${option.id}`}
                  aria-disabled={pending != null}
                  onClick={() => void handleSelect(option)}
                  className="group flex shrink-0 items-center justify-between gap-4 rounded-xl border border-ink-800/80 bg-ink-950/60 p-3.5 text-start transition-colors duration-150 hover:border-ink-700 hover:bg-ink-850/80 focus-visible:border-ember-500 focus-visible:outline-none aria-disabled:opacity-50"
                >
                  <div className="flex items-center gap-3.5 min-w-0">
                    <span className="flex h-10 w-10 shrink-0 items-center justify-center rounded-lg bg-ink-800 text-ink-300 transition-colors group-hover:bg-ink-700 group-hover:text-ink-100">
                      <Icon className="h-5 w-5" />
                    </span>
                    <div className="min-w-0">
                      <div className="text-body font-semibold text-ink-100 group-hover:text-ink-50">
                        {t(option.titleKey)}
                      </div>
                      <div className="text-secondary text-ink-400 group-hover:text-ink-300">
                        {t(option.descKey)}
                      </div>
                    </div>
                  </div>
                  <span className="shrink-0 text-ink-500 transition-colors group-hover:text-ink-200">
                    <ChevronIcon className="h-4 w-4" />
                  </span>
                </button>
              </Fragment>
            );
          })}
        </div>
      </div>
    </div>
  );
  return typeof document !== "undefined" && document.body
    ? createPortal(content, document.body)
    : content;
}
