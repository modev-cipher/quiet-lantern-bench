import type { ReactNode } from "react";

import { useI18n, type MessageKey } from "./i18n";
import type { View } from "./views";
import { GlideGroup } from "./GlideThumb";

export type SettingsSection = Extract<
  View,
  "settings" | "models" | "providers"
>;

const SECTIONS: { view: SettingsSection; testId: string; label: MessageKey }[] =
  [
    { view: "settings", testId: "settings-tab-general", label: "nav.general" },
    { view: "models", testId: "nav-models", label: "nav.voicesAndModels" },
    { view: "providers", testId: "nav-providers", label: "nav.providers" },
  ];

export function isSettingsSection(view: View): view is SettingsSection {
  return SECTIONS.some((entry) => entry.view === view);
}

function SettingsFrame({
  section,
  onSelect,
  children,
}: {
  section: SettingsSection;
  onSelect: (next: SettingsSection) => void;
  children: ReactNode;
}) {
  const { t } = useI18n();
  return (
    <div
      data-testid="settings-frame"
      className="mx-auto flex w-full max-w-panel flex-col gap-6 px-8 py-8 lg:flex-row lg:gap-10"
    >
      <div className="flex shrink-0 flex-col gap-4 lg:sticky lg:top-8 lg:w-48 lg:self-start">
        <h1 className="font-serif text-display font-semibold tracking-tight text-ink-50">
          {t("settings.title")}
        </h1>
        <nav aria-label={t("settings.sections")}>
          <GlideGroup thumbClassName="rounded-lg bg-ink-850">
            <ul className="flex flex-wrap gap-1 lg:flex-col">
              {SECTIONS.map((entry) => (
                <li key={entry.view}>
                  <button
                    type="button"
                    data-testid={entry.testId}
                    aria-current={section === entry.view ? "page" : undefined}
                    onClick={() => onSelect(entry.view)}
                    className={`relative w-full rounded-lg px-3 py-2 text-start text-secondary transition-colors duration-200 active:scale-[0.98] ${
                      section === entry.view
                        ? "font-semibold text-ink-50"
                        : "text-ink-400 hover:bg-ink-900 hover:text-ink-100"
                    }`}
                  >
                    {t(entry.label)}
                  </button>
                </li>
              ))}
            </ul>
          </GlideGroup>
        </nav>
      </div>
      <div key={section} className="animate-fade-in min-w-0 flex-1">
        {children}
      </div>
    </div>
  );
}

export default SettingsFrame;
