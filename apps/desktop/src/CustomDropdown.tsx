import { useState, useRef, useEffect, useId, type KeyboardEvent } from "react";
import { CheckIcon } from "./Icons";
import { useI18n } from "./i18n";

export interface DropdownOption {
  value: string;
  label: string;
  sublabel?: string;
  badge?: string;
  group?: string;
  testId?: string;
}

export interface CustomDropdownProps {
  id?: string;
  testId: string;
  label: string;
  value: string;
  onChange: (value: string) => void;
  options: DropdownOption[];
  placeholder?: string;
  className?: string;
  disabled?: boolean;
}

function normalized(text: string, locale: string): string {
  return text
    .normalize("NFKD")
    .replace(/\p{M}/gu, "")
    .toLocaleLowerCase(locale);
}

function searchName(text: string, locale: string): string {
  return normalized(text, locale).replace(/^[^\p{L}\p{N}]+/u, "");
}

export function CustomDropdown({
  id,
  testId,
  label,
  value,
  onChange,
  options,
  placeholder,
  className = "",
  disabled = false,
}: CustomDropdownProps) {
  const { t, locale } = useI18n();
  const [open, setOpen] = useState(false);
  const [highlighted, setHighlighted] = useState<string | null>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const buttonRef = useRef<HTMLButtonElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef({ text: "", at: 0 });
  const generatedId = useId();
  const selectId = id ?? generatedId;
  const menuId = `${selectId}-listbox`;
  const groupedOptions = new Map<string | null, DropdownOption[]>();
  for (const option of options) {
    const group = option.group ?? null;
    const items = groupedOptions.get(group) ?? [];
    items.push(option);
    groupedOptions.set(group, items);
  }
  let position = 0;
  const groups = Array.from(groupedOptions, ([name, items]) => ({
    name,
    items: items.map((option) => ({ option, index: position++ })),
  }));
  const ordered = groups.flatMap((group) =>
    group.items.map((item) => item.option),
  );
  const selectedIndex = ordered.findIndex((option) => option.value === value);
  const highlightedIndex = ordered.findIndex(
    (option) => option.value === highlighted,
  );
  const activeIndex = Math.max(
    0,
    highlightedIndex >= 0 ? highlightedIndex : selectedIndex,
  );
  const activeOption = ordered[activeIndex];
  const selectedOption = ordered[selectedIndex];
  const activeId =
    open && activeOption ? `${selectId}-option-${activeIndex}` : undefined;

  function resetSearch() {
    searchRef.current = { text: "", at: 0 };
  }

  function show(index = selectedIndex) {
    setHighlighted(ordered[Math.max(0, index)]?.value ?? null);
    resetSearch();
    setOpen(true);
  }

  function close(accept: boolean, restoreFocus = false) {
    if (accept && activeOption && activeOption.value !== value)
      onChange(activeOption.value);
    setOpen(false);
    resetSearch();
    if (restoreFocus) buttonRef.current?.focus();
  }

  useEffect(() => {
    if (disabled) setOpen(false);
  }, [disabled]);

  useEffect(() => {
    if (!open) return;
    const onPointerDown = (event: PointerEvent) => {
      if (!containerRef.current?.contains(event.target as Node)) {
        if (activeOption && activeOption.value !== value)
          onChange(activeOption.value);
        setOpen(false);
        resetSearch();
      }
    };
    document.addEventListener("pointerdown", onPointerDown);
    return () => document.removeEventListener("pointerdown", onPointerDown);
  }, [open, activeOption, value, onChange]);

  useEffect(() => {
    if (activeId) {
      listRef.current?.ownerDocument
        .getElementById(activeId)
        ?.scrollIntoView({ block: "nearest" });
    }
  }, [activeId]);

  function onKeyDown(event: KeyboardEvent<HTMLButtonElement>) {
    if (disabled || event.nativeEvent.isComposing) return;
    const key = event.key;
    const tab =
      key === "Tab" || (key === "Unidentified" && event.code === "Tab");
    if (tab) {
      if (open) close(true);
      return;
    }
    if (event.ctrlKey || event.metaKey) return;
    if (key === "Escape") {
      if (open) {
        event.preventDefault();
        event.stopPropagation();
        close(false, true);
      }
      return;
    }
    if (key === "Enter" || key === " ") {
      event.preventDefault();
      event.stopPropagation();
      if (open) close(true, true);
      else show();
      return;
    }
    if (key === "ArrowUp" && event.altKey && open) {
      event.preventDefault();
      event.stopPropagation();
      close(true, true);
      return;
    }
    if (
      ["ArrowDown", "ArrowUp", "Home", "End", "PageDown", "PageUp"].includes(
        key,
      )
    ) {
      event.preventDefault();
      event.stopPropagation();
      const base = open ? activeIndex : Math.max(0, selectedIndex);
      let next = base;
      if (key === "Home" || (!open && key === "ArrowUp")) next = 0;
      else if (key === "End") next = ordered.length - 1;
      else if (open) {
        if (key === "ArrowDown") next += 1;
        else if (key === "ArrowUp") next -= 1;
        else if (key === "PageDown") next += 10;
        else if (key === "PageUp") next -= 10;
      }
      setHighlighted(
        ordered[Math.max(0, Math.min(ordered.length - 1, next))]?.value ?? null,
      );
      if (!open) resetSearch();
      setOpen(true);
      return;
    }
    if (event.altKey || key.length !== 1) return;
    event.preventDefault();
    event.stopPropagation();
    const at = Date.now();
    const character = normalized(key, locale);
    const previous =
      at - searchRef.current.at < 700 ? searchRef.current.text : "";
    const text =
      previous && [...previous].every((part) => part === character)
        ? character
        : previous + character;
    searchRef.current = { text, at };
    const start = text.length === 1 ? activeIndex + 1 : activeIndex;
    for (let offset = 0; offset < ordered.length; offset += 1) {
      const index = (start + offset) % ordered.length;
      if (searchName(ordered[index].label, locale).startsWith(text)) {
        setHighlighted(ordered[index].value);
        break;
      }
    }
    setOpen(true);
  }

  return (
    <div
      ref={containerRef}
      className={`relative w-full ${className}`}
      data-testid={`${testId}-wrapper`}
      onBlur={(event) => {
        if (open && !event.currentTarget.contains(event.relatedTarget))
          close(true);
      }}
    >
      <button
        ref={buttonRef}
        type="button"
        role="combobox"
        id={selectId}
        data-testid={`${testId}-button`}
        aria-label={label}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={menuId}
        aria-activedescendant={activeId}
        disabled={disabled}
        onKeyDown={onKeyDown}
        onClick={() => (open ? close(false) : show())}
        className={`flex w-full items-center justify-between gap-2.5 rounded-xl border px-3.5 py-2.5 text-start text-body transition-all outline-none ${
          disabled
            ? "cursor-not-allowed border-ink-850 bg-ink-950/70 text-ink-500 opacity-60"
            : open
              ? "border-ember-500 bg-ink-900 shadow-md ring-2 ring-ember-500/20 text-ink-100"
              : "border-ink-700 bg-ink-900/90 text-ink-100 hover:border-ink-600 hover:bg-ink-900"
        }`}
      >
        <span className="flex min-w-0 flex-1 items-center gap-2">
          {selectedOption ? (
            <>
              <span className="truncate font-medium text-ink-100">
                {selectedOption.label}
              </span>
              {selectedOption.badge && (
                <span className="shrink-0 rounded px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wider bg-ink-800 text-ember-300 border border-ink-700">
                  {selectedOption.badge}
                </span>
              )}
            </>
          ) : (
            <span className="truncate text-ink-400">
              {placeholder ?? t("select.placeholder")}
            </span>
          )}
        </span>
        <svg
          className={`h-4 w-4 shrink-0 text-ink-400 transition-transform duration-200 ${open ? "rotate-180 text-ember-400" : ""}`}
          viewBox="0 0 20 20"
          fill="none"
          stroke="currentColor"
          strokeWidth="2"
          strokeLinecap="round"
          strokeLinejoin="round"
          aria-hidden="true"
        >
          <path d="m5 7.5 5 5 5-5" />
        </svg>
      </button>
      <select
        id={`${selectId}-native`}
        data-testid={testId}
        value={value}
        onChange={(event) => onChange(event.target.value)}
        disabled={disabled}
        aria-hidden="true"
        tabIndex={-1}
        className="sr-only pointer-events-none absolute opacity-0"
      >
        {options.length === 0 && (
          <option value="" disabled>
            {placeholder ?? t("select.placeholder")}
          </option>
        )}
        {groups.map((group, index) =>
          group.name ? (
            <optgroup key={index} label={group.name}>
              {group.items.map(({ option }) => (
                <option key={option.value} value={option.value}>
                  {option.label}
                  {option.sublabel ? ` (${option.sublabel})` : ""}
                </option>
              ))}
            </optgroup>
          ) : (
            group.items.map(({ option }) => (
              <option key={option.value} value={option.value}>
                {option.label}
                {option.sublabel ? ` (${option.sublabel})` : ""}
              </option>
            ))
          ),
        )}
      </select>
      {open && (
        <div
          ref={listRef}
          id={menuId}
          role="listbox"
          aria-label={label}
          data-testid={`${testId}-menu`}
          className="absolute z-50 mt-1.5 w-full max-h-64 overflow-y-auto rounded-xl border border-ink-700 bg-ink-900 p-1.5 shadow-2xl backdrop-blur-lg focus:outline-none ring-1 ring-black/40 animate-fade-in"
        >
          {ordered.length === 0 ? (
            <div
              role="option"
              aria-disabled="true"
              className="px-3 py-3 text-center text-label text-ink-400"
            >
              {t("select.empty")}
            </div>
          ) : (
            groups.map((group, groupIndex) => (
              <div
                key={groupIndex}
                role={group.name ? "group" : undefined}
                aria-label={group.name ?? undefined}
                className="space-y-0.5"
              >
                {group.name && (
                  <div
                    aria-hidden="true"
                    className="px-2.5 pt-2 pb-1 text-[11px] font-semibold uppercase tracking-wider text-ink-400 border-t first:border-t-0 border-ink-800/80"
                  >
                    {group.name}
                  </div>
                )}
                {group.items.map(({ option, index }) => (
                  <div
                    key={option.value}
                    id={`${selectId}-option-${index}`}
                    role="option"
                    aria-label={option.label}
                    aria-selected={index === activeIndex}
                    aria-describedby={
                      option.sublabel
                        ? `${selectId}-description-${index}`
                        : undefined
                    }
                    data-testid={
                      option.testId ?? `${testId}-option-${option.value}`
                    }
                    onMouseDown={(event) => event.preventDefault()}
                    onClick={() => {
                      if (option.value !== value) onChange(option.value);
                      setOpen(false);
                      resetSearch();
                      buttonRef.current?.focus();
                    }}
                    className={`flex w-full items-center justify-between gap-2 rounded-lg px-2.5 py-2 text-start text-body transition-colors cursor-pointer ${index === activeIndex ? "bg-ember-500/15 text-ember-200 font-medium" : "text-ink-200 hover:bg-ink-800/90 hover:text-ink-50"}`}
                  >
                    <div className="flex min-w-0 flex-1 flex-col">
                      <div className="flex items-center gap-2">
                        <span className="truncate">{option.label}</span>
                        {option.badge && (
                          <span className="rounded px-1.5 py-0.2 font-mono text-[9px] uppercase tracking-wider bg-ink-800 text-ink-300 border border-ink-700">
                            {option.badge}
                          </span>
                        )}
                      </div>
                      {option.sublabel && (
                        <span
                          id={`${selectId}-description-${index}`}
                          className="font-mono text-label text-ink-400 truncate"
                        >
                          {option.sublabel}
                        </span>
                      )}
                    </div>
                    {option.value === value && (
                      <CheckIcon className="h-4 w-4 shrink-0 text-ember-400" />
                    )}
                  </div>
                ))}
              </div>
            ))
          )}
        </div>
      )}
    </div>
  );
}
