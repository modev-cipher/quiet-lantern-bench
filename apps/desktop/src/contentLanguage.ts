export interface ContentLanguage {
  lang: string;
  dir: "ltr" | "rtl" | "auto";
}

type LocaleDirection = Intl.Locale & {
  getTextInfo?: () => { direction: string };
  textInfo?: { direction: string };
};

export function contentLanguage(language?: string | null): ContentLanguage {
  const unknown: ContentLanguage = { lang: "", dir: "auto" };
  const tag = language?.trim();
  if (!tag || /^(auto|unknown|und|none|mixed|mul|zxx)$/i.test(tag))
    return unknown;
  try {
    const locale: LocaleDirection = new Intl.Locale(tag);
    const script = locale.maximize().script;
    const lang = locale.toString();
    if (!script) return { lang, dir: "auto" };
    const direction =
      locale.getTextInfo?.().direction ?? locale.textInfo?.direction;
    if (direction === "ltr" || direction === "rtl")
      return { lang, dir: direction };
    if (
      ["Arab", "Hebr", "Thaa", "Nkoo", "Adlm", "Rohg", "Syrc"].includes(script)
    )
      return { lang, dir: "rtl" };
    if (
      ["Latn", "Cyrl", "Grek", "Hans", "Hant", "Jpan", "Kore", "Deva"].includes(
        script,
      )
    )
      return { lang, dir: "ltr" };
    return { lang, dir: "auto" };
  } catch {
    return unknown;
  }
}
