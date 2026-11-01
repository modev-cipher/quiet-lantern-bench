import type { LastPlace } from "./lastPlaceModel";
import type { View } from "./views";

export type AppDestination =
  { kind: "view"; view: View } | { kind: "reader"; place: LastPlace };

export interface AppHistory {
  entries: AppDestination[];
  index: number;
}

export function destinationKey(destination: AppDestination): string {
  if (destination.kind === "view") return `view:${destination.view}`;
  const place = destination.place;
  return place.kind === "pages"
    ? `pages:${place.bookId}`
    : `cast:${place.bookId}:${place.exportId}`;
}

export function pushDestination(
  history: AppHistory,
  destination: AppDestination,
): AppHistory {
  if (
    destinationKey(history.entries[history.index]) ===
    destinationKey(destination)
  )
    return history;
  const entries = [
    ...history.entries.slice(0, history.index + 1),
    destination,
  ].slice(-20);
  return { entries, index: entries.length - 1 };
}
