import type { ReadingChapter } from "./readingModel";

const SEPARATOR = "\u0000";

export interface ChapterCache {
  get: (jobId: string, chapterId: string) => ReadingChapter | undefined;
  set: (jobId: string, chapterId: string, chapter: ReadingChapter) => void;
  flush: () => void;
  size: () => number;
}

export function chapterKey(jobId: string, chapterId: string): string {
  return `${jobId.length}${SEPARATOR}${jobId}${SEPARATOR}${chapterId}`;
}

export function createChapterCache(): ChapterCache {
  const entries = new Map<string, ReadingChapter>();
  return {
    get(jobId, chapterId) {
      return entries.get(chapterKey(jobId, chapterId));
    },
    set(jobId, chapterId, chapter) {
      entries.set(chapterKey(jobId, chapterId), chapter);
    },
    flush() {
      entries.clear();
    },
    size() {
      return entries.size;
    },
  };
}
