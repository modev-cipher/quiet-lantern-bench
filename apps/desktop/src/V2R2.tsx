import { ProductionScreen } from "./V2R2Production";
import { HomeScreen, LibraryScreen } from "./V2R2Screens";
import { StudyScreen } from "./V2R2Study";

const SCREENS: Record<string, () => React.JSX.Element> = {
  resume: () => <StudyScreen resumeChip />,
  home: () => <HomeScreen />,
  study: () => <StudyScreen companionOpen />,
  focus: () => <StudyScreen focus />,
  rtl: () => <StudyScreen companionOpen arabic />,
  library: () => <LibraryScreen />,
  tray: () => <StudyScreen companionOpen tray />,
  production: () => <ProductionScreen />,
};

export function v2r2FromSearch(
  search: string,
): (() => React.JSX.Element) | null {
  if (!import.meta.env.DEV) return null;
  const params = new URLSearchParams(search);
  const key = params.get("v2r2");
  const screen = key != null ? (SCREENS[key] ?? null) : null;
  if (screen != null) {
    document.documentElement.setAttribute(
      "data-theme",
      params.get("theme") === "light" ? "light" : "dark",
    );
    if (params.get("dir") === "rtl" || key === "rtl") {
      document.documentElement.setAttribute("dir", "rtl");
    }
  }
  return screen;
}

export default function V2R2({ screen }: { screen: () => React.JSX.Element }) {
  const Screen = screen;
  return <Screen />;
}
