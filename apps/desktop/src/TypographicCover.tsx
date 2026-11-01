import { coverInitials, coverTone } from "./libraryModel";

// Each tone carries a solid base under its gradient: the cream title must
// never sit on the surface behind the cover, even where a gradient image
// does not paint (forced-colors, print) — and tools that resolve text
// grounds by background-color see the true one.
const COVER_TONES = [
  "bg-paper-base-a from-paper-tone-a to-paper-base-a",
  "bg-paper-base-b from-paper-tone-b to-paper-base-b",
  "bg-paper-base-c from-paper-tone-c to-paper-base-c",
  "bg-paper-base-d from-paper-tone-d to-paper-base-d",
  "bg-paper-base-e from-paper-tone-e to-paper-base-e",
];

const TITLE = {
  sm: "font-serif text-label leading-[1.15] font-semibold text-paper-fg line-clamp-3",
  md: "font-serif text-[19px] leading-[1.15] font-semibold text-paper-fg line-clamp-4",
  shelf:
    "font-serif text-[15px] leading-[1.18] font-semibold text-paper-fg line-clamp-4",
  lg: "font-serif text-3xl leading-[1.1] font-semibold text-paper-fg line-clamp-5",
} as const;

const PADDING = {
  sm: "p-2",
  md: "p-4",
  shelf: "py-3.5 ps-5 pe-3",
  lg: "p-7",
} as const;

function TypographicCover({
  seed,
  title,
  subtitle,
  badge,
  size = "md",
  progress = null,
  toneClassName,
}: {
  seed: string;
  title: string;
  subtitle?: string | null;
  badge?: React.ReactNode;
  size?: keyof typeof TITLE;
  toneClassName?: string;
  progress?: number | null;
}) {
  const tone = toneClassName ?? COVER_TONES[coverTone(seed)]!;
  const listened =
    progress == null || !Number.isFinite(progress) || progress <= 0
      ? null
      : Math.min(1, progress);

  return (
    <div
      className={`relative flex h-full w-full flex-col gap-2 overflow-hidden bg-gradient-to-br ${tone} ${PADDING[size]}`}
    >
      <div className="absolute inset-0 opacity-40 mix-blend-overlay [background:radial-gradient(circle_at_30%_20%,rgba(255,255,255,0.12),transparent_55%)]" />
      <div className="relative flex shrink-0 items-center justify-between">
        <span className="font-mono text-label tracking-[0.2em] text-paper-fg-muted/70 uppercase">
          {coverInitials(title) || "—"}
        </span>
        {badge}
      </div>
      <div className="relative flex min-h-0 flex-1 flex-col justify-end overflow-hidden">
        {/* The cover is an object, not document structure: every placement
            puts a real heading next to it, so its title is not one. */}
        <p
          className={TITLE[size]}
          style={{ textWrap: "balance" } as React.CSSProperties}
        >
          {title}
        </p>
        {subtitle && size !== "sm" && (
          <p
            className={`mt-2 line-clamp-2 font-serif text-paper-fg-muted/80 italic ${
              size === "lg" ? "text-sm" : "text-xs"
            }`}
          >
            {subtitle}
          </p>
        )}
      </div>
      {listened == null ? (
        <span className="relative h-0.5 w-8 shrink-0 rounded-full bg-ember-600/70" />
      ) : (
        <span
          data-testid="cover-listened"
          data-listened={listened.toFixed(3)}
          className="relative h-0.5 w-full shrink-0 overflow-hidden rounded-full bg-paper-fg/15"
        >
          <span
            className="block h-full origin-left rounded-full bg-ember-600 transition-transform duration-500"
            style={{ transform: `scaleX(${listened})` }}
          />
        </span>
      )}
    </div>
  );
}

export default TypographicCover;
