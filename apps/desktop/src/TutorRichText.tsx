// One tutor chat message, rendered from tutorTextModel: blocks with their
// own reading direction (Arabic with English terms reads right to left;
// English reads left to right — whatever the app's locale), light Markdown,
// and a smooth reveal while the reply streams. Text only: nothing in the
// message is ever interpreted as HTML.

import { useEffect, useRef, useState } from "react";
import {
  parseTutorBlocks,
  revealStep,
  trimOpenMarkers,
  type TutorInline,
} from "./tutorTextModel";

/** A bubble mounting with more text than this was already streaming. */
const REMOUNT_GLIDE_MAX_CHARS = 160;

function prefersReducedMotion(): boolean {
  return (
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches
  );
}

/**
 * The prefix of `text` to show. While `animate`, new text glides in at a
 * backlog-scaled rate ([`revealStep`]); otherwise (and under reduced
 * motion) the whole text shows at once. Text that shrinks or is replaced
 * (a corrected user line) shows in full immediately.
 */
export function useStreamReveal(text: string, streaming: boolean): string {
  // Only text that arrives after mount glides (review 2026-10-09, U3): a
  // panel reopened mid-reply shows what already streamed at once instead
  // of replaying the whole reply. A just-opened bubble holds one burst at
  // most, which still glides.
  const [shown, setShown] = useState(() =>
    streaming && text.length <= REMOUNT_GLIDE_MAX_CHARS ? 0 : text.length,
  );
  const shownRef = useRef(shown);
  shownRef.current = shown;
  const textRef = useRef(text);
  const previousRef = useRef(text);
  textRef.current = text;

  useEffect(() => {
    const previous = previousRef.current;
    previousRef.current = text;
    if (!streaming || prefersReducedMotion()) {
      setShown(text.length);
      return;
    }
    // A replaced text (a corrected line, a reused row) is not a stream
    // continuing: show it whole, whatever its length.
    if (
      shownRef.current > text.length ||
      !text.startsWith(previous.slice(0, shownRef.current))
    ) {
      setShown(text.length);
      return;
    }
    if (shownRef.current >= text.length) return;
    let frame = 0;
    let last = performance.now();
    const tick = (now: number) => {
      const next = revealStep(textRef.current, shownRef.current, now - last);
      last = now;
      setShown(next);
      if (next < textRef.current.length) frame = requestAnimationFrame(tick);
    };
    frame = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(frame);
  }, [text, streaming]);

  return streaming ? text.slice(0, Math.min(shown, text.length)) : text;
}

function Inlines({ inlines }: { inlines: TutorInline[] }) {
  return (
    <>
      {inlines.map((inline, at) => {
        switch (inline.kind) {
          case "strong":
            return (
              <strong key={at} className="font-semibold">
                {inline.text}
              </strong>
            );
          case "code":
            return (
              <code
                key={at}
                dir="ltr"
                className="tutor-code rounded bg-ink-900/60 px-1"
              >
                {inline.text}
              </code>
            );
          case "math":
            return (
              <bdi key={at} dir="ltr" className="tutor-math">
                {inline.text}
              </bdi>
            );
          case "url":
            return (
              <bdi key={at} dir="ltr" className="tutor-url">
                {inline.text}
              </bdi>
            );
          default:
            return <span key={at}>{inline.text}</span>;
        }
      })}
    </>
  );
}

export default function TutorRichText({
  text,
  animate = false,
  streaming = false,
}: {
  text: string;
  /** New text glides in (the newest reply of a live session). */
  animate?: boolean;
  /** The reply is still arriving: show the caret. */
  streaming?: boolean;
}) {
  const revealed = useStreamReveal(text, animate);
  const partial = revealed.length < text.length;
  const blocks = parseTutorBlocks(
    streaming || partial ? trimOpenMarkers(revealed) : revealed,
  );
  const caret =
    streaming || partial ? (
      <span
        aria-hidden="true"
        data-testid="tutor-caret"
        className="tutor-caret text-ink-400"
      />
    ) : null;
  if (blocks.length === 0) return caret;
  return (
    <div className="tutor-text flex min-w-0 flex-1 flex-col gap-1.5">
      {blocks.map((block, at) => {
        const tail = at === blocks.length - 1 ? caret : null;
        switch (block.kind) {
          case "table":
            return (
              <pre
                key={at}
                dir="ltr"
                data-kind="table"
                className="tutor-code overflow-x-auto whitespace-pre rounded-lg bg-ink-900/50 px-2.5 py-2 text-start"
              >
                {block.text}
                {tail}
              </pre>
            );
          case "code":
            return (
              <pre
                key={at}
                dir="ltr"
                className="tutor-code overflow-x-auto whitespace-pre rounded-lg bg-ink-900/70 px-2.5 py-2 text-start"
              >
                {block.text}
                {tail}
              </pre>
            );
          case "heading":
            return (
              <p
                key={at}
                dir={block.dir}
                data-dir={block.dir}
                className="text-start font-semibold"
              >
                <Inlines inlines={block.inlines} />
                {tail}
              </p>
            );
          case "item":
            return (
              <div
                key={at}
                dir={block.dir}
                data-dir={block.dir}
                className="flex gap-1.5 text-start"
                style={
                  block.depth > 0
                    ? { marginInlineStart: `${block.depth * 1.1}rem` }
                    : undefined
                }
              >
                <span
                  aria-hidden="true"
                  className="shrink-0 tabular-nums text-ink-400"
                >
                  {block.ordered ? <bdi dir="ltr">{block.marker}</bdi> : "•"}
                </span>
                <span className="min-w-0 flex-1">
                  {block.ordered && (
                    <span className="sr-only">{block.marker} </span>
                  )}
                  <Inlines inlines={block.inlines} />
                  {tail}
                </span>
              </div>
            );
          default:
            return (
              <p
                key={at}
                dir={block.dir}
                data-dir={block.dir}
                className={`text-start ${block.continued === true ? "-mt-1.5" : ""}`}
              >
                <Inlines inlines={block.inlines} />
                {tail}
              </p>
            );
        }
      })}
    </div>
  );
}
