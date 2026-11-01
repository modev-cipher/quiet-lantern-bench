import { describe, expect, it } from "vitest";

import golden from "../e2e/fixtures/reading-timeline.golden.json";
import type { ReadingChapter, ReadingWord } from "./readingModel";
import { locatedWordCount, locateWords, tokenise } from "./readingTokens";

function words(...spans: Array<[string, number, number]>): ReadingWord[] {
  return spans.map(([w, startMs, endMs]) => ({ w, startMs, endMs, rect: null }));
}

describe("locateWords", () => {
  it("finds each word by a forward search, so repeats cannot collide", () => {
    const located = locateWords(
      "the cat and the hat",
      words(["the", 0, 100], ["the", 200, 300]),
    );

    expect(located.map((span) => span.start)).toEqual([0, 12]);
  });

  it("matches case-insensitively without moving the text", () => {
    const located = locateWords("The Cat", words(["the", 0, 100]));

    expect(located[0]).toEqual({ start: 0, end: 3, wordIdx: 0 });
  });

  it("skips a word the text does not contain rather than guessing a place", () => {
    const located = locateWords(
      "one three",
      words(["one", 0, 100], ["two", 100, 200], ["three", 200, 300]),
    );

    expect(located.map((span) => span.wordIdx)).toEqual([0, 2]);
  });

  it("never returns overlapping spans", () => {
    const located = locateWords(
      "sentences sentence",
      words(["sentences", 0, 100], ["sentence", 100, 200]),
    );

    for (let i = 1; i < located.length; i += 1) {
      expect(located[i].start).toBeGreaterThanOrEqual(located[i - 1].end);
    }
  });

  it("prefers a whole-word match over a substring of a longer word", () => {
    const located = locateWords("concatenate the cat", words(["cat", 0, 100]));

    expect(
      located[0].start,
      "matching inside 'concatenate' would highlight three glyphs of an unrelated word",
    ).toBe(16);
  });

  it("still matches when no boundary-aligned occurrence exists, so CJK keeps working", () => {
    const located = locateWords("第一句读出来", words(["一句", 0, 100]));

    expect(located).toHaveLength(1);
    expect(located[0].start).toBe(1);
  });

  it("ignores an empty token, which can own no glyphs", () => {
    expect(locateWords("a b", words(["", 0, 0], ["b", 0, 100]))).toHaveLength(1);
  });
});

describe("tokenise", () => {
  it("covers the text exactly once, with the gaps carried as plain runs", () => {
    const text = "The first sentence reads aloud.";
    const chapter = (golden.chapters as ReadingChapter[])[0];
    const tokens = tokenise(text, chapter.segments[0].words);

    expect(tokens.map((token) => token.text).join("")).toBe(text);
  });

  it("assigns a word index only to the runs a word owns", () => {
    const tokens = tokenise("one two", words(["one", 0, 100]));

    expect(tokens).toEqual([
      { text: "one", wordIdx: 0 },
      { text: " two", wordIdx: null },
    ]);
  });

  it("keeps the trailing punctuation outside the highlighted run", () => {
    const tokens = tokenise("aloud.", words(["aloud", 0, 100]));

    expect(tokens).toEqual([
      { text: "aloud", wordIdx: 0 },
      { text: ".", wordIdx: null },
    ]);
  });

  it("returns the whole text as one unhighlighted run when nothing aligns", () => {
    expect(tokenise("nothing here", [])).toEqual([
      { text: "nothing here", wordIdx: null },
    ]);
  });

  it("emits every index the golden's fresh segments carry", () => {
    for (const chapter of golden.chapters as ReadingChapter[]) {
      for (const segment of chapter.segments) {
        if (segment.words.length === 0) continue;
        const tokens = tokenise(segment.text, segment.words);
        const indices = tokens
          .map((token) => token.wordIdx)
          .filter((index): index is number => index != null);

        expect(indices).toEqual([...indices].sort((a, b) => a - b));
        expect(new Set(indices).size).toBe(indices.length);
        expect(
          locatedWordCount(segment.text, segment.words),
          `${segment.segmentId} loses a word the highlight needs`,
        ).toBe(indices.length);
      }
    }
  });

  it("carries a CJK segment as one run rather than garbling it", () => {
    const tokens = tokenise("第一句读出来。", words(["第一句", 0, 100]));

    expect(tokens.map((token) => token.text).join("")).toBe("第一句读出来。");
    expect(tokens[0]).toEqual({ text: "第一句", wordIdx: 0 });
  });

  it("matches all 40 words in Attention segment 0 without dropping intermediate words", () => {
    const text =
      "When we look at how information is represented, we often find that the length of the sequence, which we call 'n', is actually shorter than the dimensionality of the representation, which we call 'd'.";
    const segmentWords = [
      "When", "we", "look", "at", "how", "information", "is", "represented", ",", "we",
      "often", "find", "that", "the", "length", "of", "the", "sequence", ",", "which",
      "we", "call", "'n", "'", ",", "is", "actually", "shorter", "than", "the",
      "dimensionality", "of", "the", "representation", ",", "which", "we", "call", "'d", "'",
    ].map((w, i) => [w, i * 100, (i + 1) * 100] as [string, number, number]);

    const tokens = tokenise(text, words(...segmentWords));
    const matched = tokens.filter((t) => t.wordIdx !== null);
    expect(matched).toHaveLength(40);
  });

  it("prevents pure punctuation tokens from leaping over intervening words", () => {
    const text = "A simple sentence without comma until much later, end.";
    const tokenWords = words(["A", 0, 100], [",", 100, 200], ["simple", 200, 300], ["sentence", 300, 400]);
    const tokens = tokenise(text, tokenWords);
    const matchedIndices = tokens.map((t) => t.wordIdx).filter((idx): idx is number => idx !== null);

    expect(matchedIndices).toEqual([0, 2, 3]);
  });
});

