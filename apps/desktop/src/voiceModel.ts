import type { VoiceProfileSummary } from "./jobFormModel";
import { t, type MessageKey } from "./i18n";

export type { VoiceProfileSummary };

export interface VoiceMeta {
  language: string;
  langCode: string;
  flag: string;
  gender: "female" | "male";
  shortName: string;
}

const LANGUAGES: Record<
  string,
  { language: MessageKey; langCode: string; flag: string }
> = {
  a: { language: "voice.lang.americanEnglish", langCode: "en", flag: "🇺🇸" },
  b: { language: "voice.lang.britishEnglish", langCode: "en", flag: "🇬🇧" },
  e: { language: "voice.lang.spanish", langCode: "es", flag: "🇪🇸" },
  f: { language: "voice.lang.french", langCode: "fr", flag: "🇫🇷" },
  h: { language: "voice.lang.hindi", langCode: "hi", flag: "🇮🇳" },
  i: { language: "voice.lang.italian", langCode: "it", flag: "🇮🇹" },
  j: { language: "voice.lang.japanese", langCode: "ja", flag: "🇯🇵" },
  p: { language: "voice.lang.portuguese", langCode: "pt", flag: "🇧🇷" },
  z: { language: "voice.lang.chinese", langCode: "zh", flag: "🇨🇳" },
};

export function voiceMeta(
  presetVoiceId: string | null | undefined,
): VoiceMeta | null {
  if (!presetVoiceId) return null;
  const [prefix, ...rest] = presetVoiceId.split("_");
  const raw = rest.join("_");
  if (prefix == null || prefix.length !== 2 || raw.length === 0) return null;
  const lang = LANGUAGES[prefix[0]!];
  if (lang == null) return null;
  const gender =
    prefix[1] === "f" ? "female" : prefix[1] === "m" ? "male" : null;
  if (gender == null) return null;
  return {
    ...lang,
    language: t(lang.language),
    gender,
    shortName: raw[0]!.toUpperCase() + raw.slice(1),
  };
}

export function voiceLabel(profile: VoiceProfileSummary): string {
  const meta = voiceMeta(profile.presetVoiceId);
  if (meta == null) return profile.name;
  return `${meta.flag} ${meta.shortName} · ${meta.language} (${t(meta.gender === "female" ? "voice.gender.female" : "voice.gender.male")})`;
}

export interface VoiceGroup {
  language: string;
  flag: string;
  voices: VoiceProfileSummary[];
}

export function groupVoices(profiles: VoiceProfileSummary[]): VoiceGroup[] {
  const groups = new Map<string, VoiceGroup>();
  const order: string[] = [];
  for (const profile of profiles) {
    const meta = voiceMeta(profile.presetVoiceId);
    const other = t("voice.lang.other");
    const language = meta?.language ?? other;
    const flag = meta?.flag ?? "🎙";
    if (!groups.has(language)) {
      groups.set(language, { language, flag, voices: [] });
      order.push(language);
    }
    groups.get(language)!.voices.push(profile);
  }
  for (const group of groups.values()) {
    group.voices.sort((a, b) =>
      voiceLabel(a).localeCompare(voiceLabel(b), "en"),
    );
  }
  order.sort((a, b) => {
    const other = t("voice.lang.other");
    if (a === other) return 1;
    if (b === other) return -1;
    return a.localeCompare(b, "en");
  });
  return order.map((language) => groups.get(language)!);
}
