import type { ReadingWord } from "./readingModel";

export interface TextToken {
  text: string;
  wordIdx: number | null;
}

function foldForSearch(value: string): string {
  return value.toLowerCase();
}

const WORD_CHARACTER = /[\p{L}\p{N}]/u;
const CJK = /[\p{Script=Han}\p{Script=Hiragana}\p{Script=Katakana}\p{Script=Hangul}]/u;

function findAligned(haystack: string, needle: string, from: number): number {
  if (!needle) return -1;
  const hasWordChars = WORD_CHARACTER.test(needle);
  if (!hasWordChars) {
    const at = haystack.indexOf(needle, from);
    if (at < 0) return -1;
    const between = haystack.slice(from, at);
    if (WORD_CHARACTER.test(between)) return -1;
    return at;
  }

  const checkLeft = WORD_CHARACTER.test(needle[0]);
  const checkRight = WORD_CHARACTER.test(needle[needle.length - 1]);

  let at = haystack.indexOf(needle, from);
  while (at >= 0) {
    const leftOk =
      !checkLeft ||
      at === 0 ||
      !WORD_CHARACTER.test(haystack[at - 1]) ||
      CJK.test(haystack[at - 1]) ||
      CJK.test(needle[0]);
    const rightOk =
      !checkRight ||
      at + needle.length >= haystack.length ||
      !WORD_CHARACTER.test(haystack[at + needle.length]) ||
      CJK.test(haystack[at + needle.length]) ||
      CJK.test(needle[needle.length - 1]);
    if (leftOk && rightOk) {
      return at;
    }
    at = haystack.indexOf(needle, at + 1);
  }
  return -1;
}

export function locateWords(
  text: string,
  words: ReadingWord[],
): Array<{ start: number; end: number; wordIdx: number }> {
  const haystack = foldForSearch(text);
  const found: Array<{ start: number; end: number; wordIdx: number }> = [];
  let cursor = 0;
  for (let wordIdx = 0; wordIdx < words.length; wordIdx += 1) {
    const needle = foldForSearch(words[wordIdx].w.trim());
    if (needle.length === 0) continue;
    const at = findAligned(haystack, needle, cursor);
    if (at < 0) continue;
    found.push({ start: at, end: at + needle.length, wordIdx });
    cursor = at + needle.length;
  }
  return found;
}

export function tokenise(text: string, words: ReadingWord[]): TextToken[] {
  const located = locateWords(text, words);
  const tokens: TextToken[] = [];
  let cursor = 0;
  for (const span of located) {
    if (span.start > cursor) {
      tokens.push({ text: text.slice(cursor, span.start), wordIdx: null });
    }
    tokens.push({
      text: text.slice(span.start, span.end),
      wordIdx: span.wordIdx,
    });
    cursor = span.end;
  }
  if (cursor < text.length) {
    tokens.push({ text: text.slice(cursor), wordIdx: null });
  }
  return tokens;
}

export function locatedWordCount(text: string, words: ReadingWord[]): number {
  return locateWords(text, words).length;
}
