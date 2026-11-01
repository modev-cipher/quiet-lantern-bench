import { setupStatus, type CatalogModelEntry } from "./modelCatalogModel";
import type { View } from "./views";
import type { MessageKey } from "./i18n";

export interface ChecklistItem {
  id: string;
  label: MessageKey;
  done: boolean;
  optional: boolean;
  target: View;
}

export interface SetupChecklist {
  items: ChecklistItem[];
  remainingRequired: number;
  allRequiredDone: boolean;
}

const CAPABILITY_ITEMS: Record<
  string,
  { id: string; label: MessageKey; optional: boolean }
> = {
  tts: { id: "voice", label: "setup.voice", optional: false },
  llm: { id: "llm", label: "setup.llm", optional: true },
};

export function buildSetupChecklist(params: {
  models: CatalogModelEntry[];
  installed: Set<string>;
  bookCount: number;
  jobCount: number;
}): SetupChecklist {
  const items: ChecklistItem[] = [];

  for (const capability of setupStatus(params.models, params.installed)) {
    const meta = CAPABILITY_ITEMS[capability.task];
    if (!meta) continue;
    items.push({
      id: meta.id,
      label: meta.label,
      done: capability.ready,
      optional: meta.optional,
      target: "models",
    });
  }

  items.push({
    id: "import",
    label: "setup.import",
    done: params.bookCount > 0,
    optional: false,
    target: "library",
  });

  items.push({
    id: "cast",
    label: "setup.cast",
    done: params.jobCount > 0,
    optional: false,
    target: "jobs",
  });

  const remainingRequired = items.filter(
    (item) => !item.optional && !item.done,
  ).length;

  return {
    items,
    remainingRequired,
    allRequiredDone: remainingRequired === 0,
  };
}
