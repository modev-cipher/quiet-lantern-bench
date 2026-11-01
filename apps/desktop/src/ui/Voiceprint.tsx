export default function Voiceprint({
  bars,
  label,
}: {
  bars: readonly number[];
  label: string;
}) {
  return (
    <>
      <span
        aria-hidden="true"
        data-testid="tutor-voiceprint"
        className="ui-voiceprint"
      >
        {bars.map((bar, at) => (
          <b key={at} style={{ height: `${Math.max(2, Math.round((bar / 255) * 14))}px` }} />
        ))}
      </span>
      <span className="sr-only">{label}: </span>
    </>
  );
}
