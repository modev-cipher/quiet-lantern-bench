import { useSyncExternalStore } from "react";

import { getSnapshot, subscribe, type ThemeState } from "./themeStore";

export function useTheme(): ThemeState {
  return useSyncExternalStore(subscribe, getSnapshot);
}
