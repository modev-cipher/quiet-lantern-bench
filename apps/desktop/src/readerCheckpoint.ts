import type { CompanionTab } from "./StudyCompanion";
import type { ReadingMode } from "./readingPageModel";
import type { VoiceNavigation } from "./voiceSessionModel";
import type { PageFit } from "./readingPageFit";

export interface NavigationCheckpoint {
  consumed: number;
  latest: VoiceNavigation | null;
  held: boolean;
}

export interface ReaderCheckpoint {
  page: number | null;
  scroll: number;
  mode: ReadingMode;
  fit: PageFit;
  zoom: number;
  companionTab: CompanionTab;
  focusMode: boolean;
  sessionId: string | null;
  navigation: NavigationCheckpoint;
}
