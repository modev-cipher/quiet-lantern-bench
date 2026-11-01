export type OrbKind = "listen" | "speak" | "think" | "rest" | "warn" | "idle";

export default function Orb({
  kind,
  size = 28,
}: {
  kind: OrbKind;
  size?: number;
}) {
  return (
    <span
      aria-hidden="true"
      className="ui-orb"
      data-kind={kind}
      style={size === 28 ? undefined : { width: size, height: size }}
    >
      <i className="ui-orb-aura" />
      <i className="ui-orb-core" />
    </span>
  );
}
