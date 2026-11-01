import { MicIcon, MicOffIcon } from "../Icons";

interface Props {
  muted: boolean;
  disabled?: boolean;
  label: string;
  onToggle: () => void;
  testId?: string;
}

export default function MicToggle({
  muted,
  disabled = false,
  label,
  onToggle,
  testId,
}: Props) {
  const state = disabled ? "off" : muted ? "muted" : "open";
  return (
    <button
      type="button"
      className="ui-mic"
      data-state={state}
      data-testid={testId}
      aria-pressed={muted}
      aria-label={label}
      title={label}
      disabled={disabled}
      onClick={onToggle}
    >
      {muted || disabled ? <MicOffIcon /> : <MicIcon />}
    </button>
  );
}
