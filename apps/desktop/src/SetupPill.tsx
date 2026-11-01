import { useEffect, useRef, useState } from "react";

import { CheckIcon, ChevronIcon, CircleIcon } from "./Icons";
import type { ChecklistItem, SetupChecklist } from "./setupChecklist";
import type { View } from "./views";
import { useI18n } from "./i18n";

const HIDE_KEY = "txtcaster.setup.hidden";
const RING = 2 * Math.PI * 6;

function readHidden(): boolean {
  try {
    return sessionStorage.getItem(HIDE_KEY) === "1";
  } catch {
    return false;
  }
}

function ChecklistRow({
  item,
  onNavigate,
}: {
  item: ChecklistItem;
  onNavigate: (view: View) => void;
}) {
  const { t } = useI18n();
  const content = (
    <>
      <span
        className={`flex h-5 w-5 shrink-0 items-center justify-center rounded-full ${
          item.done ? "bg-wash-green text-wash-green-ink" : "text-ink-500"
        }`}
      >
        {item.done ? (
          <CheckIcon className="h-3.5 w-3.5" />
        ) : (
          <CircleIcon className="h-4 w-4" />
        )}
      </span>
      <span
        className={`min-w-0 flex-1 ${
          item.done ? "text-ink-500 line-through" : "text-ink-200"
        }`}
      >
        {t(item.label)}
      </span>
      {item.optional && !item.done && (
        <span className="shrink-0 text-label text-ink-500">
          {t("setup.optional")}
        </span>
      )}
    </>
  );
  if (item.done) {
    return (
      <div
        data-testid={`checklist-item-${item.id}`}
        data-done="true"
        className="flex items-center gap-2.5 rounded-md px-2 py-1.5 text-secondary"
      >
        {content}
      </div>
    );
  }
  return (
    <button
      type="button"
      data-testid={`checklist-item-${item.id}`}
      data-done="false"
      onClick={() => onNavigate(item.target)}
      className="flex w-full items-center gap-2.5 rounded-md px-2 py-1.5 text-start text-secondary transition-colors duration-200 hover:bg-ink-800/60"
    >
      {content}
    </button>
  );
}

function SetupPill({
  checklist,
  onNavigate,
}: {
  checklist: SetupChecklist;
  onNavigate: (view: View) => void;
}) {
  const { t } = useI18n();
  const [open, setOpen] = useState(false);
  const [hidden, setHidden] = useState(readHidden);
  const [justFinished, setJustFinished] = useState(false);
  const wasPending = useRef(!checklist.allRequiredDone);
  const rootRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (checklist.allRequiredDone && wasPending.current) {
      wasPending.current = false;
      setJustFinished(true);
      const id = window.setTimeout(() => setJustFinished(false), 2600);
      return () => window.clearTimeout(id);
    }
    if (!checklist.allRequiredDone) wasPending.current = true;
    return undefined;
  }, [checklist.allRequiredDone]);

  useEffect(() => {
    if (!open) return;
    const onPointerDown = (event: PointerEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener("pointerdown", onPointerDown);
    return () => document.removeEventListener("pointerdown", onPointerDown);
  }, [open]);

  if (hidden) return null;
  if (checklist.allRequiredDone && !justFinished) return null;

  const required = checklist.items.filter((item) => !item.optional);
  const done = required.filter((item) => item.done).length;
  const fraction = required.length === 0 ? 1 : done / required.length;
  const next = checklist.items.find((item) => !item.done && !item.optional);

  return (
    <div
      ref={rootRef}
      data-testid="setup-pill"
      className="setup-pill pointer-events-auto relative"
      onKeyDown={(event) => {
        if (event.key === "Escape" && open) {
          event.stopPropagation();
          setOpen(false);
        }
      }}
    >
      <div className="shadow-overlay flex items-center gap-2.5 rounded-full border border-ink-800 bg-ink-900 py-1.5 ps-2.5 pe-1.5 text-secondary">
        <svg
          viewBox="0 0 16 16"
          className="h-4 w-4 shrink-0 -rotate-90"
          aria-hidden="true"
        >
          <circle
            cx="8"
            cy="8"
            r="6"
            fill="none"
            stroke="var(--color-ink-700)"
            strokeWidth="2.5"
          />
          <circle
            cx="8"
            cy="8"
            r="6"
            fill="none"
            stroke="var(--color-ember-500)"
            strokeWidth="2.5"
            strokeLinecap="round"
            strokeDasharray={RING.toFixed(2)}
            strokeDashoffset={(RING * (1 - fraction)).toFixed(2)}
            className="transition-[stroke-dashoffset] duration-700 ease-out"
          />
        </svg>
        {justFinished || next == null ? (
          <span
            data-testid="setup-all-set"
            className="pe-2 font-medium text-ink-100"
          >
            {t("setup.allSet")}
          </span>
        ) : (
          <>
            <span className="min-w-0 truncate text-ink-300">
              <span className="font-semibold text-ink-100">
                {t("setup.next")}
              </span>{" "}
              {t(next.label)}
            </span>
            <button
              type="button"
              data-testid="setup-show-me"
              onClick={() => onNavigate(next.target)}
              className="shrink-0 rounded-full px-2.5 py-0.5 font-medium text-ember-400 transition-colors duration-200 hover:bg-ink-850 hover:text-ember-300"
            >
              {t("setup.showMe")}
            </button>
            <button
              type="button"
              data-testid="setup-toggle"
              aria-expanded={open}
              aria-label={t("setup.progress", { done, total: required.length })}
              title={t("setup.progress", { done, total: required.length })}
              onClick={() => setOpen((value) => !value)}
              className="flex h-7 w-7 shrink-0 items-center justify-center rounded-full text-ink-400 transition-colors duration-200 hover:bg-ink-850 hover:text-ink-100"
            >
              <ChevronIcon
                className={`h-4 w-4 transition-transform duration-300 ${open ? "rotate-90" : "-rotate-90"}`}
              />
            </button>
          </>
        )}
      </div>
      {open && (
        <div
          data-testid="setup-checklist"
          className="setup-pop shadow-overlay absolute end-0 bottom-full z-30 mb-2 flex w-72 flex-col gap-0.5 rounded-xl border border-ink-800 bg-ink-900 p-2"
        >
          <p className="px-2 pt-1 pb-1.5 text-label font-semibold text-ink-400">
            {t("setup.progress", { done, total: required.length })}
          </p>
          {checklist.items.map((item) => (
            <ChecklistRow key={item.id} item={item} onNavigate={onNavigate} />
          ))}
          <button
            type="button"
            data-testid="setup-hide"
            onClick={() => {
              try {
                sessionStorage.setItem(HIDE_KEY, "1");
              } catch {
                return setHidden(true);
              }
              setHidden(true);
            }}
            className="mt-1 self-start rounded-md px-2 py-1 text-label text-ink-500 transition-colors duration-200 hover:text-ink-200"
          >
            {t("setup.hide")}
          </button>
        </div>
      )}
    </div>
  );
}

export default SetupPill;
