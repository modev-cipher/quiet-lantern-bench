import { AudioIcon, CheckIcon, CloseIcon, PlayIcon } from "./Icons";
import { StudyScreen } from "./V2R2Study";

const STAGES = [
  { label: "Ingest", state: "done" },
  { label: "Layout", state: "done" },
  { label: "Vision", state: "done" },
  { label: "Script", state: "done" },
  { label: "Synthesize", state: "active" },
  { label: "Align", state: "pending" },
  { label: "Render", state: "pending" },
] as const;

const QUIET_ACTION =
  "rounded-lg border border-ink-800 px-2.5 py-1 text-label font-medium text-ink-300 transition-colors duration-200 hover:text-ink-100 active:scale-[0.98]";
const SOLID_ACTION =
  "flex items-center gap-1.5 rounded-lg bg-ink-800 px-2.5 py-1.5 text-label font-semibold text-ink-100 transition-colors duration-200 hover:bg-ink-700 active:scale-[0.98]";

function SectionLabel({ text }: { text: string }) {
  return (
    <p className="text-label font-semibold tracking-[0.14em] text-ink-500 uppercase">
      {text}
    </p>
  );
}

function StageRail() {
  return (
    <ol className="flex items-center gap-2">
      {STAGES.map((stage) => (
        <li key={stage.label} className="flex items-center gap-1.5">
          {stage.state === "done" ? (
            <CheckIcon className="h-3.5 w-3.5 text-ink-500" />
          ) : (
            <span
              className={`h-2 w-2 rounded-full ${
                stage.state === "active" ? "bg-ember-500" : "bg-ink-700"
              }`}
            />
          )}
          <span
            className={`font-mono text-label ${
              stage.state === "active"
                ? "font-medium text-ember-400"
                : "text-ink-500"
            }`}
          >
            {stage.label}
          </span>
        </li>
      ))}
    </ol>
  );
}

export function ProductionScreen() {
  return (
    <div className="relative h-screen overflow-hidden">
      <StudyScreen companionOpen />
      <div className="absolute inset-0 z-40 flex items-start justify-center overflow-y-auto bg-ink-950/85 p-8">
        <section
          role="dialog"
          aria-modal="true"
          aria-label="Production"
          data-testid="v2r2-production"
          className="animate-modal-in shadow-overlay w-full max-w-panel rounded-2xl border border-ink-800 bg-ink-900"
        >
          <header className="flex items-center gap-4 border-b border-ink-800 px-7 py-4">
            <h1 className="font-serif text-title font-semibold text-ink-50">
              Production
            </h1>
            <span className="flex items-center gap-2 text-label text-ink-400">
              <span className="h-2 w-2 rounded-full bg-wash-green-ink" />
              Engine ready
            </span>
            <span className="flex-1" />
            <p className="text-label text-ink-500">
              One cast runs at a time · defaults in Settings
            </p>
            <button
              type="button"
              aria-label="Close production"
              className="rounded-lg p-1.5 text-ink-400 transition-colors duration-200 hover:text-ink-100 active:scale-[0.98]"
            >
              <CloseIcon className="h-4 w-4" />
            </button>
          </header>
          <div className="flex flex-col gap-6 px-7 py-6">
            <div>
              <SectionLabel text="Now" />
              <div className="mt-2.5 rounded-xl border border-ink-800 bg-ink-850 p-4">
                <div className="flex items-baseline justify-between gap-4">
                  <p className="text-body font-semibold text-ink-50">
                    Arabic Grammar Primer · Chapters 6–7 · Narrator
                  </p>
                  <p className="font-mono text-label text-ink-400">
                    43% · about 12 min left
                  </p>
                </div>
                <div className="mt-3">
                  <StageRail />
                </div>
                <span className="mt-3 block h-1 rounded-full bg-ink-800">
                  <span className="block h-1 w-[43%] rounded-full bg-ember-500" />
                </span>
                <div className="mt-3 flex items-center gap-2">
                  <p className="min-w-0 flex-1 truncate text-secondary text-ink-300">
                    Speaking Chapter 6 · segment 118 of 214
                  </p>
                  <button type="button" className={QUIET_ACTION}>
                    Pause
                  </button>
                  <button type="button" className={QUIET_ACTION}>
                    Cancel
                  </button>
                </div>
              </div>
            </div>
            <div>
              <SectionLabel text="Waiting" />
              <div className="mt-2.5 flex flex-col gap-2">
                <div className="flex items-center gap-3 rounded-xl border border-ink-800 px-4 py-3">
                  <div className="min-w-0 flex-1">
                    <p className="text-secondary font-medium text-ink-100">
                      The Physics of Sound · Chapters 3–9 · Narrator
                    </p>
                    <p className="text-label text-ink-400">
                      Queued — waiting for the current cast · set to run
                      overnight
                    </p>
                  </div>
                  <button type="button" className={QUIET_ACTION}>
                    Cancel
                  </button>
                </div>
                <div className="flex items-center gap-3 rounded-xl border border-ink-800 px-4 py-3">
                  <div className="min-w-0 flex-1">
                    <p className="text-secondary font-medium text-ink-100">
                      Poems of al-Andalus · Sections 5–6 · Teacher
                    </p>
                    <p className="text-label text-ink-400">
                      Paused itself after memory contention · attempt 2 ·
                      finished work is kept
                    </p>
                  </div>
                  <button type="button" className={SOLID_ACTION}>
                    Resume
                  </button>
                </div>
              </div>
            </div>
            <div>
              <SectionLabel text="Needs attention" />
              <div className="mt-2.5 flex flex-col gap-2">
                <div className="flex items-center gap-3 rounded-xl border border-wash-red-ink/40 bg-wash-red px-4 py-3">
                  <div className="min-w-0 flex-1">
                    <p className="text-secondary font-medium text-wash-red-ink">
                      Notes on Optics · Chapters 5–8 · failed
                    </p>
                    <p className="text-label text-wash-red-ink">
                      The narration voice model is missing — install it in
                      Settings → Models, then retry from the checkpoint.
                    </p>
                  </div>
                  <button type="button" className={QUIET_ACTION}>
                    Get the model
                  </button>
                  <button type="button" className={QUIET_ACTION}>
                    Retry
                  </button>
                </div>
                <div className="flex items-center gap-3 rounded-xl border border-ink-800 px-4 py-3">
                  <div className="min-w-0 flex-1">
                    <p className="text-secondary font-medium text-ink-100">
                      Notes on Optics · Chapters 1–4 · audio stale
                    </p>
                    <p className="text-label text-ink-400">
                      The book was edited after this cast — play is held until a
                      re-render, so you never hear mismatched audio.
                    </p>
                  </div>
                  <button type="button" className={SOLID_ACTION}>
                    Re-render
                  </button>
                </div>
              </div>
            </div>
            <div>
              <SectionLabel text="Done" />
              <div className="mt-2.5 flex items-center gap-3 rounded-xl border border-ink-800 px-4 py-3">
                <div className="min-w-0 flex-1">
                  <p className="text-secondary font-medium text-ink-100">
                    Letters on Rhetoric · all 12 chapters · finished yesterday
                  </p>
                  <p className="text-label text-ink-400">
                    2 exports · accuracy check passed · 3 segments worth a look
                  </p>
                </div>
                <button type="button" className={SOLID_ACTION}>
                  <PlayIcon className="h-3.5 w-3.5" /> Play
                </button>
                <button type="button" className={QUIET_ACTION}>
                  Review
                </button>
                <button type="button" className={QUIET_ACTION}>
                  Send to phone
                </button>
              </div>
            </div>
            <div>
              <SectionLabel text="History" />
              <div className="mt-2.5 flex flex-col gap-1">
                <div className="flex items-center gap-3 rounded-lg px-2 py-1.5">
                  <AudioIcon className="h-4 w-4 shrink-0 text-ink-500" />
                  <p className="min-w-0 flex-1 truncate text-label text-ink-400">
                    Poems of al-Andalus · Sections 1–4 · done Aug 24
                  </p>
                </div>
                <div className="flex items-center gap-3 rounded-lg px-2 py-1.5">
                  <CloseIcon className="h-4 w-4 shrink-0 text-ink-500" />
                  <p className="min-w-0 flex-1 truncate text-label text-ink-400">
                    The Physics of Sound · Chapter 1 draft · cancelled Aug 23 —
                    its export was kept
                  </p>
                </div>
                <button
                  type="button"
                  className="self-start rounded-lg px-2 py-1 text-label font-medium text-ink-500 transition-colors duration-200 hover:text-ink-200"
                >
                  Older casts…
                </button>
              </div>
            </div>
          </div>
        </section>
      </div>
    </div>
  );
}
