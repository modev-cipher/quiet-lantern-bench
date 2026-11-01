import { describe, expect, it } from "vitest";

import {
  NEVER_PERSIST_NOTE,
  personalizeLocked,
  personalizeNoteOf,
  preferenceTopicLabel,
  type LearningPrivacy,
} from "./learningPrivacyModel";

const normal = (personalizeEnabled: boolean): LearningPrivacy => ({
  personalizeEnabled,
  neverPersist: false,
});

const neverPersist = (personalizeEnabled: boolean): LearningPrivacy => ({
  personalizeEnabled,
  neverPersist: true,
});

describe("personalizeLocked", () => {
  it("locks the toggle exactly when never-persist is active", () => {
    expect(personalizeLocked(normal(true))).toBe(false);
    expect(personalizeLocked(normal(false))).toBe(false);
    expect(personalizeLocked(neverPersist(true))).toBe(true);
    expect(personalizeLocked(neverPersist(false))).toBe(true);
  });
});

describe("personalizeNoteOf", () => {
  it("explains the lock under never-persist and says nothing otherwise", () => {
    expect(personalizeNoteOf(neverPersist(true))).toBe(NEVER_PERSIST_NOTE);
    expect(personalizeNoteOf(neverPersist(false))).toBe(NEVER_PERSIST_NOTE);
    expect(personalizeNoteOf(normal(true))).toBeNull();
    expect(personalizeNoteOf(normal(false))).toBeNull();
  });
});

describe("the lock encodes never-persist-wins (owner ruling D-2)", () => {
  it("a toggle showing On under never-persist is locked, so the position cannot be mistaken for behavior", () => {
    expect(personalizeLocked(neverPersist(true))).toBe(true);
    expect(personalizeNoteOf(neverPersist(true))).toBe(NEVER_PERSIST_NOTE);
  });
});

describe("preferenceTopicLabel", () => {
  it("names the topics the tutor saves under and keeps any other key as written", () => {
    expect(preferenceTopicLabel("pace")).toBe("Pace");
    expect(preferenceTopicLabel(" Style ")).toBe("Explanation style");
    expect(preferenceTopicLabel("equations")).toBe("equations");
  });

  it("keeps keys that share a name with built-in object members as written", () => {
    for (const key of [
      "constructor",
      "__proto__",
      "toString",
      "hasOwnProperty",
    ]) {
      expect(preferenceTopicLabel(key)).toBe(key);
    }
  });
});
