import { describe, expect, it } from "vitest";

import { readSource } from "./tests/readSource";

const SURFACES = ["MiniPlayer.tsx"] as const;

function body(name: string): string {
  return readSource(new URL(`./${name}`, import.meta.url));
}

describe("a playback failure never removes the player's own retry affordance", () => {
  it("keeps an AudioPlayer mounted on every surface that owns playback", () => {
    for (const name of SURFACES) {
      const source = body(name);
      const mounted = [
        ...source.matchAll(/<AudioPlayer\s+variant="(\w+)"/g),
      ].map((match) => match[1]);
      expect(
        mounted.length,
        `${name} no longer mounts an AudioPlayer, so this pin says nothing about it`,
      ).toBe(1);
      expect(
        source,
        `${name} must mount <AudioPlayer variant="${mounted[0]}" /> unconditionally. get_media_url \
rejects with a bare string for a stale export AND for a transient sqlx or canonicalize failure, so \
errorMessage cannot distinguish them; gating the player on it hides the retry a transient error needs`,
      ).toContain(`<AudioPlayer variant="${mounted[0]}"`);
      expect(
        source,
        `${name} gates its AudioPlayer behind a condition, which hides the retry a transient error needs`,
      ).not.toMatch(/(&&|\?|:)\s*<AudioPlayer\b/);
    }
  });

  it("may not infer permanence from the presence of a message", () => {
    for (const name of SURFACES) {
      expect(
        body(name),
        `${name} derives a refusal flag from errorMessage. The backend has one error type and three \
causes, only one permanent; deciding permanence from prose is how a transient SQLITE_BUSY turns \
into a player the user cannot restart`,
      ).not.toMatch(/errorMessage\s*[!=]==?\s*null/);
    }
  });

  it("still shows what went wrong on the strip, with the same fallback order", () => {
    expect(body("MiniPlayer.tsx")).toContain('data-testid="player-error"');
    for (const name of SURFACES) {
      expect(body(name)).toContain(
        'const playbackError = errorMessage ?? (error ? t("player.playbackFailed") : null);',
      );
    }
  });
});
