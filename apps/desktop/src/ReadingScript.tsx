import { forwardRef } from "react";

import { contentLanguage } from "./contentLanguage";
import ReadingFallback from "./ReadingFallback";
import ReadingUnderline from "./ReadingUnderline";
import type { ReadingSegment } from "./readingModel";
import { WORD_ATTRIBUTE } from "./readingHighlight";
import { tokenise } from "./readingTokens";

interface ReadingScriptProps {
  segment: ReadingSegment | null;
  language?: string | null;
  underlineRef: React.RefObject<HTMLDivElement | null> | null;
  renderCount: number;
  onRepair: (() => void) | null;
}

const ReadingScript = forwardRef<HTMLDivElement, ReadingScriptProps>(
  function ReadingScript(
    { segment, language, underlineRef, renderCount, onRepair },
    ref,
  ) {
    const followable = segment != null && segment.words.length > 0;
    const tokens = followable ? tokenise(segment.text, segment.words) : [];

    return (
      <div className="flex w-full flex-col gap-4">
        <div
          ref={ref}
          data-testid="reading-script"
          data-render-count={renderCount}
          data-segment={segment?.segmentId ?? ""}
          className="font-serif text-2xl leading-relaxed tracking-tight text-ink-100"
        >
          {segment == null ? null : !followable ? (
            <ReadingFallback
              segment={segment}
              language={language}
              onRepair={onRepair}
            />
          ) : (
            <p {...contentLanguage(language)}>
              {tokens.map((token, position) =>
                token.wordIdx == null ? (
                  <span key={position}>{token.text}</span>
                ) : (
                  <span
                    key={position}
                    {...{ [WORD_ATTRIBUTE]: token.wordIdx }}
                    className="rounded-sm px-0.5 transition-colors duration-75"
                  >
                    {token.text}
                  </span>
                ),
              )}
            </p>
          )}
        </div>
        {underlineRef != null && (
          <ReadingUnderline underlineRef={underlineRef} />
        )}
      </div>
    );
  },
);

export default ReadingScript;
