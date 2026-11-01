function Switch({
  checked,
  onChange,
  label,
  disabled = false,
  testId,
}: {
  checked: boolean;
  onChange: (next: boolean) => void;
  label: string;
  disabled?: boolean;
  testId?: string;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      aria-disabled={disabled || undefined}
      disabled={disabled}
      data-testid={testId}
      onClick={() => onChange(!checked)}
      className={`switch-track relative inline-flex h-6 w-10 shrink-0 items-center rounded-full border transition-colors duration-200 disabled:cursor-not-allowed disabled:opacity-50 ${
        checked
          ? "border-ember-500 bg-ember-500"
          : "border-ink-700 bg-ink-800 hover:border-ink-600"
      }`}
    >
      <span
        aria-hidden="true"
        className={`switch-knob absolute top-1/2 h-4.5 w-4.5 rounded-full bg-on-accent shadow-sm ${
          checked ? "switch-knob-on" : ""
        }`}
      />
    </button>
  );
}

export default Switch;
