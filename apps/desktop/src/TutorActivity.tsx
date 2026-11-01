// The tutor's activity strip and step trail (review 2026-10-09, U1/U2).
//
// Everything here renders runtime facts only: the glyph and the words come
// from the `Activity` events the session actor emits when it is really in
// that state (voiceSessionModel `activityMessage`), and the step trail is
// the list of tool steps the runtime reported for this reply. An unknown
// wait shows the neutral "thinking" orbit — never a specific action.
//
// Motion is decorative and quiet: CSS keyframes on small inline SVGs
// (index.css `tutor-glyph-*`), all stopped under prefers-reduced-motion.
// Colors are the ink/ember tokens, so light and dark themes both hold.

import type { ReactNode } from "react";
import { useI18n } from "./i18n";
import { CheckIcon, CloseIcon } from "./Icons";
import {
  activityMessage,
  stepMessage,
  type ActivityMessage,
  type ActivityStep,
  type VoiceActivity,
} from "./voiceSessionModel";

/** Seconds before the strip shows its clock (a short wait needs none). */
export const ACTIVITY_CLOCK_AFTER_S = 4;

type Glyph =
  "orbit" | "compose" | "page" | "look" | "read" | "facet" | "retry" | "pulse";

const TOOL_GLYPHS: Record<string, Glyph> = {
  jump_to_page: "page",
  jump_to_chapter: "page",
  replay_previous_page: "page",
  go_to_passage: "page",
  open_passage: "page",
  look_at_page: "look",
  get_context_around_position: "read",
  get_context_for_range: "read",
  summarize_section: "read",
  search_book: "read",
};

function glyphOf(activity: VoiceActivity): Glyph {
  switch (activity.state) {
    case "thinking":
      return activity.detail === "after_tool" ? "compose" : "orbit";
    case "tool":
      return TOOL_GLYPHS[activity.detail] ?? "facet";
    case "retrying":
    case "fallback":
      return "retry";
    case "connecting":
    case "reconnecting":
      return "pulse";
    default:
      return "orbit";
  }
}

type Translate = ReturnType<typeof useI18n>["t"];

export function activityText(message: ActivityMessage, t: Translate): string {
  return t(message.key, message.params);
}

function stepText(step: ActivityStep, t: Translate): string {
  const done = activityText(stepMessage(step), t);
  return step.failed ? t("voice.step.failed", { step: done }) : done;
}

/** One small animated mark per kind of real work. */
function ActivityGlyph({ glyph }: { glyph: Glyph }) {
  const body: Record<Glyph, ReactNode> = {
    // A spark circling a still centre: an open, unspecified wait.
    orbit: (
      <>
        <circle cx="8" cy="8" r="5.25" className="tutor-glyph-track" />
        <circle cx="8" cy="2.75" r="1.6" className="tutor-glyph-spark" />
        <circle cx="8" cy="8" r="1.25" className="tutor-glyph-core" />
      </>
    ),
    // Lines writing themselves: the explanation is being prepared.
    compose: (
      <>
        <path d="M3 5h10" className="tutor-glyph-line" />
        <path d="M3 8h8" className="tutor-glyph-line" />
        <path d="M3 11h6" className="tutor-glyph-line" />
      </>
    ),
    // A leaf folding over: a page turn.
    page: (
      <>
        <rect
          x="3"
          y="2.5"
          width="9"
          height="11"
          rx="1.25"
          className="tutor-glyph-track"
        />
        <path d="M7.5 2.5h4.5v11h-4.5z" className="tutor-glyph-leaf" />
      </>
    ),
    // Viewfinder corners closing on the page: examining what is on it.
    look: (
      <>
        <path d="M2.5 5.5v-3h3" className="tutor-glyph-corner" />
        <path d="M10.5 2.5h3v3" className="tutor-glyph-corner" />
        <path d="M13.5 10.5v3h-3" className="tutor-glyph-corner" />
        <path d="M5.5 13.5h-3v-3" className="tutor-glyph-corner" />
        <circle cx="8" cy="8" r="1.6" className="tutor-glyph-core" />
      </>
    ),
    // A scan line passing over text: reading the book.
    read: (
      <>
        <path d="M3 4.5h10M3 8h10M3 11.5h7" className="tutor-glyph-track" />
        <path d="M2.5 3v10" className="tutor-glyph-scan" />
      </>
    ),
    // Facets turning: some other tool step.
    facet: (
      <path
        d="M8 2.5l4.75 2.75v5.5L8 13.5l-4.75-2.75v-5.5z"
        className="tutor-glyph-facet"
      />
    ),
    // An arc chasing itself: the provider was busy, trying again.
    retry: (
      <>
        <circle cx="8" cy="8" r="5.25" className="tutor-glyph-track" />
        <path
          d="M8 2.75a5.25 5.25 0 0 1 5.25 5.25"
          className="tutor-glyph-arc"
        />
      </>
    ),
    // A ring breathing out: a connection is being made.
    pulse: (
      <>
        <circle cx="8" cy="8" r="2" className="tutor-glyph-core" />
        <circle cx="8" cy="8" r="5.25" className="tutor-glyph-ring" />
      </>
    ),
  };
  return (
    <svg
      viewBox="0 0 16 16"
      width="16"
      height="16"
      aria-hidden="true"
      focusable="false"
      data-glyph={glyph}
      className={`tutor-glyph tutor-glyph-${glyph} shrink-0`}
    >
      {body[glyph]}
    </svg>
  );
}

/**
 * The strip shown while a reply is owed. `seconds` is how long the tutor
 * has been in the current state; the clock is visual only (a ticking
 * number in a live region would be read out every second).
 */
export function TutorActivityStrip({
  activity,
  message,
  seconds,
}: {
  activity: VoiceActivity;
  message: ActivityMessage;
  seconds: number;
}) {
  const { t } = useI18n();
  // The running tool is the label; the trail lists the steps before it.
  const finished =
    activity.state === "tool" ? activity.steps.slice(0, -1) : activity.steps;
  return (
    <li
      data-testid="voice-session-activity"
      data-activity={activity.state}
      className="tutor-activity me-auto flex max-w-[92%] items-start gap-2.5 rounded-xl border border-ink-800 bg-ink-900/40 px-3 py-2 text-label text-ink-300"
    >
      <span className="mt-px text-ember-400">
        <ActivityGlyph glyph={glyphOf(activity)} />
      </span>
      <span className="flex min-w-0 flex-col gap-0.5">
        <span key={activity.seq} className="tutor-activity-label">
          {activityText(message, t)}
        </span>
        {finished.length > 0 && <TutorSteps steps={finished} />}
      </span>
      {seconds >= ACTIVITY_CLOCK_AFTER_S && (
        <span
          aria-hidden="true"
          data-testid="voice-session-activity-clock"
          className="ms-auto shrink-0 tabular-nums text-ink-500"
        >
          {t("voice.activity.elapsed", { seconds })}
        </span>
      )}
    </li>
  );
}

/** The steps a reply took, in the past tense, quietly. */
export function TutorSteps({ steps }: { steps: ActivityStep[] }) {
  const { t } = useI18n();
  if (steps.length === 0) return null;
  return (
    <ul
      aria-label={t("voice.steps.label")}
      data-testid="tutor-steps"
      className="flex flex-wrap items-center gap-x-2 gap-y-0.5 text-label text-ink-500"
    >
      {steps.map((step, at) => (
        <li
          key={at}
          data-failed={step.failed ? "true" : undefined}
          className={`flex items-center gap-1 ${step.failed ? "text-wash-red-ink" : ""}`}
        >
          {step.status != null ? (
            <span aria-hidden="true">·</span>
          ) : step.failed ? (
            <CloseIcon className="h-3 w-3" />
          ) : (
            <CheckIcon className="h-3 w-3" />
          )}
          <span>{stepText(step, t)}</span>
        </li>
      ))}
    </ul>
  );
}

/**
 * A persistent, visually hidden status region: mounted once, so screen
 * readers announce each change of what the tutor is doing (a region that
 * mounts with its text is often not announced at all).
 */
export function TutorActivityAnnouncer({
  activity,
  live,
}: {
  activity: VoiceActivity;
  live: boolean;
}) {
  const { t } = useI18n();
  const message = live ? activityMessage(activity) : null;
  return (
    <p
      role="status"
      aria-live="polite"
      data-testid="voice-session-activity-announcer"
      className="sr-only"
    >
      {message != null ? activityText(message, t) : ""}
    </p>
  );
}
