import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import {
  TutorActivityAnnouncer,
  TutorActivityStrip,
  TutorSteps,
} from "./TutorActivity";
import {
  activityMessage,
  IDLE_ACTIVITY,
  type VoiceActivity,
} from "./voiceSessionModel";

// Review 2026-10-09, U1/U2: the strip says what the runtime reported, with
// its target; the clock never sits in a live region; one persistent region
// announces state changes; a reply keeps the steps that led to it.

function strip(activity: VoiceActivity, seconds = 0): string {
  return renderToStaticMarkup(
    createElement(TutorActivityStrip, {
      activity,
      message: activityMessage(activity)!,
      seconds,
    }),
  );
}

describe("TutorActivityStrip", () => {
  it("names the page a page turn acts on, with a page glyph", () => {
    const html = strip({
      ...IDLE_ACTIVITY,
      state: "tool",
      detail: "jump_to_page",
      target: "23",
      steps: [{ tool: "jump_to_page", target: "23", failed: false }],
      seq: 2,
    });
    expect(html).toContain("Turning to page");
    expect(html).toContain("23");
    expect(html).toContain('data-glyph="page"');
    expect(
      html,
      "the running step is the label, not a finished step",
    ).not.toContain("Turned to document page");
    expect(html).not.toContain('role="status"');
  });

  it("lists the finished steps while preparing the reply", () => {
    const html = strip({
      ...IDLE_ACTIVITY,
      state: "thinking",
      detail: "after_tool",
      steps: [
        { tool: "jump_to_page", target: "23", failed: false },
        { tool: "look_at_page", target: "23", failed: false },
      ],
      seq: 4,
    });
    expect(html).toContain("Preparing the explanation");
    expect(html).toContain('data-glyph="compose"');
    expect(html).toContain("Turned to document page");
    expect(html).toContain("Looked at document page");
  });

  it("an unknown wait shows only the neutral orbit", () => {
    const html = strip({ ...IDLE_ACTIVITY, state: "thinking", seq: 1 });
    expect(html).toContain('data-glyph="orbit"');
    expect(html).toContain("Thinking");
    expect(html).not.toContain("tutor-steps");
  });

  it("shows the clock after a noticeable wait, hidden from screen readers", () => {
    const quick = strip({ ...IDLE_ACTIVITY, state: "thinking", seq: 1 }, 2);
    expect(quick).not.toContain("voice-session-activity-clock");
    const slow = strip({ ...IDLE_ACTIVITY, state: "thinking", seq: 1 }, 9);
    expect(slow).toMatch(
      /aria-hidden="true" data-testid="voice-session-activity-clock"/,
    );
  });
});

describe("TutorActivityAnnouncer", () => {
  it("is always mounted as a polite status region", () => {
    const idle = renderToStaticMarkup(
      createElement(TutorActivityAnnouncer, {
        activity: IDLE_ACTIVITY,
        live: true,
      }),
    );
    expect(idle).toMatch(/role="status" aria-live="polite"/);
    const busy = renderToStaticMarkup(
      createElement(TutorActivityAnnouncer, {
        activity: { ...IDLE_ACTIVITY, state: "reconnecting", seq: 1 },
        live: true,
      }),
    );
    expect(busy).toContain("Reconnecting");
  });
});

describe("TutorSteps", () => {
  it("marks a failed step as failed, not as done", () => {
    const html = renderToStaticMarkup(
      createElement(TutorSteps, {
        steps: [{ tool: "jump_to_page", target: "999", failed: true }],
      }),
    );
    expect(html).toContain('data-failed="true"');
    expect(html).toContain("didn");
    expect(html).toContain("m6 6 12 12M18 6 6 18");
    expect(html).not.toContain("✓");
  });
});
