import { useRef, type KeyboardEvent, type ReactNode } from "react";

export interface SegmentOption<V extends string> {
  value: V;
  label: string;
  icon?: ReactNode;
  disabled?: boolean;
  hot?: boolean;
  testId?: string;
}

interface Props<V extends string> {
  options: readonly SegmentOption<V>[];
  value: V;
  onChange: (value: V) => void;
  label: string;
  describedBy?: string;
  testId?: string;
  dataMode?: string;
}

export function nextSegment<V extends string>(
  options: readonly SegmentOption<V>[],
  from: V,
  key: string,
  rtl: boolean,
): V | null {
  const enabled = options.filter((option) => option.disabled !== true);
  if (enabled.length === 0) return null;
  const at = enabled.findIndex((option) => option.value === from);
  const step =
    key === "ArrowRight"
      ? rtl
        ? -1
        : 1
      : key === "ArrowLeft"
        ? rtl
          ? 1
          : -1
        : key === "ArrowDown"
          ? 1
          : key === "ArrowUp"
            ? -1
            : null;
  if (step != null) {
    const base = at === -1 ? 0 : at;
    return enabled[(base + step + enabled.length) % enabled.length].value;
  }
  if (key === "Home") return enabled[0].value;
  if (key === "End") return enabled[enabled.length - 1].value;
  return null;
}

export default function SegmentedSwitch<V extends string>({
  options,
  value,
  onChange,
  label,
  describedBy,
  testId,
  dataMode,
}: Props<V>) {
  const groupRef = useRef<HTMLDivElement | null>(null);
  const selected = Math.max(
    0,
    options.findIndex((option) => option.value === value),
  );

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const group = groupRef.current;
    if (group == null) return;
    const rtl = getComputedStyle(group).direction === "rtl";
    const next = nextSegment(options, value, event.key, rtl);
    if (next == null) return;
    event.preventDefault();
    if (next !== value) onChange(next);
    group
      .querySelector<HTMLButtonElement>(`[data-value="${next}"]`)
      ?.focus();
  };

  return (
    <div
      ref={groupRef}
      role="radiogroup"
      aria-label={label}
      aria-describedby={describedBy}
      data-testid={testId}
      data-mode={dataMode}
      className="ui-seg"
      onKeyDown={onKeyDown}
    >
      <span
        aria-hidden="true"
        className="ui-seg-thumb"
        style={{
          width: `calc((100% - 6px) / ${options.length})`,
          transform: `translateX(calc(${selected * 100}% * var(--ui-dir, 1)))`,
        }}
      />
      {options.map((option) => {
        const checked = option.value === value;
        return (
          <button
            key={option.value}
            type="button"
            role="radio"
            aria-checked={checked}
            tabIndex={checked ? 0 : -1}
            disabled={option.disabled}
            data-value={option.value}
            data-hot={option.hot === true ? "true" : undefined}
            data-testid={option.testId}
            className="ui-seg-option"
            onClick={() => {
              if (!checked) onChange(option.value);
            }}
          >
            {option.icon}
            <span>{option.label}</span>
          </button>
        );
      })}
    </div>
  );
}
