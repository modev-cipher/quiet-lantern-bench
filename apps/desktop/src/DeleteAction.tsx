import { useEffect, useRef, useState } from "react";

import { TrashIcon } from "./Icons";

const SIZES = {
  sm: {
    trigger:
      "flex h-7 shrink-0 items-center gap-1 whitespace-nowrap rounded-lg border border-transparent px-2 text-secondary text-ink-400 transition-colors duration-200 hover:bg-wash-red hover:text-wash-red-ink active:scale-[0.97]",
    confirm:
      "flex h-7 items-center rounded-md bg-wash-red-ink px-2.5 whitespace-nowrap text-label font-medium text-ink-950 transition-colors hover:bg-wash-red-ink/90",
    cancel:
      "flex h-7 items-center rounded-md px-2 whitespace-nowrap text-label text-ink-400 transition-colors hover:text-ink-200",
    question: "text-label font-medium text-ink-200",
    icon: "h-4 w-4",
  },
  md: {
    trigger:
      "flex shrink-0 items-center gap-2 whitespace-nowrap rounded-lg border border-wash-red-ink/30 px-3.5 py-2 text-sm font-medium text-wash-red-ink transition-colors duration-200 hover:bg-wash-red active:scale-[0.97]",
    confirm:
      "rounded-lg bg-wash-red-ink px-3.5 py-2 whitespace-nowrap text-sm font-semibold text-ink-950 transition-colors hover:bg-wash-red-ink/90 active:scale-[0.97]",
    cancel:
      "rounded-xl px-3 py-2 whitespace-nowrap text-sm text-ink-300 transition-colors hover:text-ink-100",
    question: "text-sm font-medium text-ink-200",
    icon: "h-4 w-4",
  },
} as const;

export function DeleteAction({
  testId,
  label = "Delete",
  confirmLabel = "Confirm delete",
  cancelLabel = "Keep",
  question,
  title,
  size = "sm",
  requireConfirm = true,
  disabled = false,
  onConfirm,
}: {
  testId: string;
  label?: string | null;
  confirmLabel?: string;
  cancelLabel?: string;
  question?: string;
  title?: string;
  size?: keyof typeof SIZES;
  requireConfirm?: boolean;
  /** F11-U6 J7.4: a refused delete renders the trigger inert with its title
   * saying why; the confirm pair never opens and `onConfirm` never fires. */
  disabled?: boolean;
  onConfirm: () => void;
}) {
  const [confirming, setConfirming] = useState(false);
  const confirmRef = useRef<HTMLButtonElement | null>(null);
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  const style = SIZES[size];

  useEffect(() => {
    if (confirming) confirmRef.current?.focus();
  }, [confirming]);

  const cancel = () => {
    setConfirming(false);
    requestAnimationFrame(() => triggerRef.current?.focus());
  };

  if (confirming) {
    return (
      <div
        className="flex min-w-0 flex-wrap items-center gap-2"
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.stopPropagation();
            cancel();
          }
        }}
      >
        {question && (
          <span role="status" className={style.question}>
            {question}
          </span>
        )}
        <button
          ref={confirmRef}
          type="button"
          data-testid={`confirm-delete-${testId}`}
          onClick={onConfirm}
          className={style.confirm}
        >
          {confirmLabel}
        </button>
        <button
          type="button"
          data-testid={`cancel-delete-${testId}`}
          onClick={cancel}
          className={style.cancel}
        >
          {cancelLabel}
        </button>
      </div>
    );
  }

  return (
    <button
      ref={triggerRef}
      type="button"
      data-testid={`delete-${testId}`}
      aria-label={title || label || "Delete"}
      title={title}
      disabled={disabled}
      aria-disabled={disabled || undefined}
      onClick={() => {
        if (disabled) return;
        if (requireConfirm) setConfirming(true);
        else onConfirm();
      }}
      className={`${style.trigger} ${disabled ? "cursor-not-allowed opacity-50 hover:bg-transparent" : ""}`}
    >
      <TrashIcon className={style.icon} />
      {label !== null && label !== "" && label}
    </button>
  );
}

export default DeleteAction;
