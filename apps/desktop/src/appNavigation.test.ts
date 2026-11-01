import { expect, it } from "vitest";
import { pushDestination, type AppHistory } from "./appNavigation";

it("retains a bounded history, suppresses duplicates and clears Forward on a new branch", () => {
  const initial: AppHistory = {
    entries: [{ kind: "view", view: "library" }],
    index: 0,
  };
  expect(pushDestination(initial, initial.entries[0])).toBe(initial);
  const reader = pushDestination(initial, {
    kind: "reader",
    place: { kind: "pages", bookId: "A" },
  });
  const jobs = pushDestination(reader, { kind: "view", view: "jobs" });
  const branch = pushDestination(
    { ...jobs, index: 1 },
    { kind: "view", view: "settings" },
  );
  expect(branch.entries).toEqual([
    initial.entries[0],
    reader.entries[1],
    { kind: "view", view: "settings" },
  ]);
  let bounded = initial;
  for (let i = 0; i < 40; i++)
    bounded = pushDestination(bounded, {
      kind: "reader",
      place: { kind: "pages", bookId: String(i) },
    });
  expect(bounded.entries).toHaveLength(20);
  expect(bounded.index).toBe(19);
});
