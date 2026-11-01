use std::path::Path;

use sqlx::SqlitePool;

use crate::catalog::{Catalog, CatalogEntry, DownloadFile, ModelFormat, ModelTask};
use crate::storage::models::ModelInstall;
use crate::storage::{now_ms, voice};

pub const MULTI_VOICE: &str = "multi_voice";
pub const VOICE_CLONING: &str = "voice_cloning";
pub const KOKORO_MODEL_ID: &str = "kokoro-82m";
pub const KOKORO_DESKTOP_LANGUAGES: [&str; 3] = ["en", "ja", "zh"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceInfo {
    pub voice_id: String,
    pub name: String,
    pub language: String,
    pub lang_code: String,
    pub gender: String,
}

pub fn parse_voice_id(voice_id: &str) -> Option<VoiceInfo> {
    let (prefix, raw_name) = voice_id.split_once('_')?;
    let mut chars = prefix.chars();
    let lang = chars.next()?;
    let gender = match chars.next()? {
        'f' => "female",
        'm' => "male",
        _ => return None,
    };
    if chars.next().is_some() || raw_name.is_empty() {
        return None;
    }
    let (language, lang_code) = match lang {
        'a' => ("American English", "en"),
        'b' => ("British English", "en"),
        'e' => ("Spanish", "es"),
        'f' => ("French", "fr"),
        'h' => ("Hindi", "hi"),
        'i' => ("Italian", "it"),
        'j' => ("Japanese", "ja"),
        'p' => ("Portuguese", "pt"),
        'z' => ("Chinese", "zh"),
        _ => return None,
    };
    let mut name_chars = raw_name.chars();
    let first = name_chars.next()?;
    let name = format!("{}{}", first.to_uppercase(), name_chars.collect::<String>());
    Some(VoiceInfo {
        voice_id: voice_id.to_string(),
        name,
        language: language.to_string(),
        lang_code: lang_code.to_string(),
        gender: gender.to_string(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageMismatch {
    pub book: String,
    pub voice_speaks: Option<String>,
}

pub fn normalize_lang(raw: &str) -> Option<String> {
    let code = raw
        .trim()
        .split(['-', '_'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    (!code.is_empty()).then_some(code)
}

pub fn language_advisory(
    book_language: Option<&str>,
    preset_voice_id: Option<&str>,
    engine_languages: &[String],
) -> Option<LanguageMismatch> {
    let book = book_language.and_then(normalize_lang)?;
    if let Some(info) = preset_voice_id.and_then(parse_voice_id) {
        if info.lang_code == book {
            return None;
        }
        return Some(LanguageMismatch {
            book,
            voice_speaks: Some(info.lang_code),
        });
    }
    if engine_languages.is_empty() {
        return None;
    }
    if engine_languages
        .iter()
        .any(|lang| normalize_lang(lang).as_deref() == Some(book.as_str()))
    {
        return None;
    }
    Some(LanguageMismatch {
        book,
        voice_speaks: None,
    })
}

pub fn sample_sentence(lang_code: &str) -> &'static str {
    match lang_code {
        "ar" => "مرحبًا، هكذا يبدو صوتي عندما أقرأ كتبك بصوت عالٍ.",
        "cs" => "Ahoj, takhle zní můj hlas, když předčítám vaše knihy nahlas.",
        "de" => "Hallo, so klinge ich, wenn ich deine Bücher laut vorlese.",
        "es" => "Hola, así sueno cuando leo tus libros en voz alta.",
        "fr" => "Bonjour, voici ma voix quand je lis vos livres à haute voix.",
        "hi" => "नमस्ते, आपकी किताबें पढ़ते समय मेरी आवाज़ ऐसी लगती है।",
        "hu" => "Helló, így hangzik a hangom, amikor felolvasom a könyveidet.",
        "it" => "Ciao, questa è la mia voce quando leggo i tuoi libri ad alta voce.",
        "ja" => "こんにちは、本を読み上げるときの私の声はこんな感じです。",
        "ko" => "안녕하세요, 책을 소리 내어 읽을 때 제 목소리는 이렇습니다.",
        "nl" => "Hallo, zo klink ik als ik je boeken hardop voorlees.",
        "pl" => "Cześć, tak brzmi mój głos, kiedy czytam twoje książki na głos.",
        "pt" => "Olá, é assim que eu soo quando leio seus livros em voz alta.",
        "ru" => "Здравствуйте, вот так звучит мой голос, когда я читаю ваши книги вслух.",
        "tr" => "Merhaba, kitaplarınızı sesli okurken sesim böyle çıkıyor.",
        "zh" => "你好，这是我朗读你的书时的声音。",
        _ => "Hi there — this is how I sound reading your books aloud.",
    }
}

pub fn preview_sentence_for(voice_id: &str) -> &'static str {
    match parse_voice_id(voice_id) {
        Some(info) => sample_sentence(&info.lang_code),
        None => sample_sentence("en"),
    }
}

pub fn preview_sentence_for_lang(voice_id: &str, preferred_lang: Option<&str>) -> &'static str {
    if let Some(lang) = preferred_lang {
        return sample_sentence(lang);
    }
    preview_sentence_for(voice_id)
}

pub fn preset_profile_id(family: &str, voice_id: &str) -> String {
    let slug = voice_id.replace(['_', ' '], "-").to_ascii_lowercase();
    format!("preset-{family}-{slug}")
}

fn format_extensions(format: ModelFormat) -> &'static [&'static str] {
    match format {
        ModelFormat::Gguf => &["gguf"],
        ModelFormat::Onnx => &["onnx"],
        ModelFormat::Pytorch => &["pth", "pt", "bin", "safetensors"],
        ModelFormat::Ct2 => &["ct2"],
        ModelFormat::Unknown => &[],
    }
}

fn file_stem(dest_path: &str) -> Option<&str> {
    let name = dest_path.rsplit('/').next()?;
    let (stem, _ext) = name.rsplit_once('.')?;
    if stem.is_empty() {
        return None;
    }
    Some(stem)
}

fn has_extension(dest_path: &str, extensions: &[&str]) -> bool {
    dest_path
        .rsplit('.')
        .next()
        .is_some_and(|ext| extensions.iter().any(|e| ext.eq_ignore_ascii_case(e)))
}

pub fn single_voice_source(entry: &CatalogEntry) -> Option<(&str, &DownloadFile)> {
    let extensions = format_extensions(entry.format);
    let file = entry
        .download
        .files
        .iter()
        .find(|f| has_extension(&f.dest_path, extensions))
        .or_else(|| entry.download.files.first())?;
    Some((file_stem(&file.dest_path)?, file))
}

fn single_voice_profile(entry: &CatalogEntry, stem: &str) -> voice::VoiceProfile {
    voice::VoiceProfile {
        id: preset_profile_id(&entry.family, &stem.to_ascii_lowercase()),
        name: entry.display_name.clone(),
        engine: entry.id.clone(),
        kind: "preset".to_string(),
        preset_voice_id: Some(stem.to_string()),
        reference_wav_path: None,
        params_json: None,
        created_at: now_ms(),
    }
}

pub fn is_listed_local_voice_engine(engine: &str) -> bool {
    engine != crate::providers::CLOUD_VOICE_ENGINE
        && !crate::model_manager::RETIRED_MODEL_IDS.contains(&engine)
}

pub fn is_listed_local_voice(engine: &str, preset_voice_id: Option<&str>) -> bool {
    if !is_listed_local_voice_engine(engine) {
        return false;
    }
    if engine != KOKORO_MODEL_ID {
        return true;
    }
    match preset_voice_id.and_then(parse_voice_id) {
        Some(info) => KOKORO_DESKTOP_LANGUAGES.contains(&info.lang_code.as_str()),
        None => true,
    }
}

pub async fn sync_preset_voice_profiles(
    pool: &SqlitePool,
    catalog: &Catalog,
    installs: &[ModelInstall],
    models_root: &Path,
) -> Result<usize, sqlx::Error> {
    let mut synced = 0;
    for entry in &catalog.models {
        if entry.task != ModelTask::Tts {
            continue;
        }
        let Some(install) = installs.iter().find(|i| i.model_id == entry.id) else {
            continue;
        };
        let install_dir = models_root.join(&install.path);
        if !entry.capabilities.iter().any(|c| c == MULTI_VOICE) {
            if entry.capabilities.iter().any(|c| c == VOICE_CLONING) {
                continue;
            }
            let Some((stem, file)) = single_voice_source(entry) else {
                continue;
            };
            if !install_dir.join(&file.dest_path).is_file() {
                continue;
            }
            voice::insert_voice_profile_if_missing(pool, &single_voice_profile(entry, stem))
                .await?;
            synced += 1;
            continue;
        }
        for file in &entry.download.files {
            let Some(stem) = file
                .dest_path
                .strip_prefix("voices/")
                .and_then(|rest| rest.strip_suffix(".pt"))
            else {
                continue;
            };
            let Some(info) = parse_voice_id(stem) else {
                continue;
            };
            if !install_dir.join(&file.dest_path).is_file() {
                continue;
            }
            let profile = voice::VoiceProfile {
                id: preset_profile_id(&entry.family, stem),
                name: format!("{} · {} ({})", info.name, info.language, info.gender),
                engine: entry.id.clone(),
                kind: "preset".to_string(),
                preset_voice_id: Some(info.voice_id),
                reference_wav_path: None,
                params_json: None,
                created_at: now_ms(),
            };
            voice::insert_voice_profile_if_missing(pool, &profile).await?;
            synced += 1;
        }
    }
    Ok(synced)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purged_and_cloud_engines_are_not_listed_as_local_voices() {
        assert!(is_listed_local_voice_engine("kokoro-82m"));
        assert!(is_listed_local_voice_engine("chatterbox-0.5b"));
        for engine in [
            "xtts-v2",
            "piper-en-us-lessac-medium",
            "orpheus-3b",
            "f5-tts",
            "cloud",
        ] {
            assert!(!is_listed_local_voice_engine(engine), "{engine}");
        }
    }

    #[test]
    fn parses_language_gender_and_name() {
        let info = parse_voice_id("af_heart").expect("info");
        assert_eq!(info.name, "Heart");
        assert_eq!(info.language, "American English");
        assert_eq!(info.lang_code, "en");
        assert_eq!(info.gender, "female");

        let info = parse_voice_id("jm_kumo").expect("info");
        assert_eq!(info.name, "Kumo");
        assert_eq!(info.language, "Japanese");
        assert_eq!(info.lang_code, "ja");
        assert_eq!(info.gender, "male");

        let info = parse_voice_id("zf_xiaobei").expect("info");
        assert_eq!(info.language, "Chinese");
        assert_eq!(info.lang_code, "zh");
    }

    #[test]
    fn rejects_malformed_voice_ids() {
        for bad in ["", "heart", "xf_heart", "ax_heart", "af_", "abf_heart"] {
            assert!(parse_voice_id(bad).is_none(), "accepted: {bad}");
        }
    }

    #[test]
    fn preset_id_matches_existing_default() {
        assert_eq!(
            preset_profile_id("kokoro", "af_heart"),
            "preset-kokoro-af-heart"
        );
    }

    #[test]
    fn preview_sentences_match_voice_language() {
        assert!(preview_sentence_for("af_heart").starts_with("Hi there"));
        assert!(preview_sentence_for("bf_alice").starts_with("Hi there"));
        assert!(preview_sentence_for("jf_alpha").contains("こんにちは"));
        assert!(preview_sentence_for("zf_xiaobei").contains("你好"));
        assert!(preview_sentence_for("ef_dora").starts_with("Hola"));
        assert!(preview_sentence_for("unknown").starts_with("Hi there"));
    }

    fn langs(codes: &[&str]) -> Vec<String> {
        codes.iter().map(|c| c.to_string()).collect()
    }

    #[test]
    fn an_unknown_book_language_never_raises_the_advisory() {
        assert_eq!(
            language_advisory(None, Some("af_heart"), &langs(&["en"])),
            None
        );
        assert_eq!(language_advisory(Some("  "), None, &langs(&["en"])), None);
    }

    #[test]
    fn a_voice_that_speaks_the_book_language_raises_nothing() {
        let kokoro = langs(&KOKORO_DESKTOP_LANGUAGES);
        assert_eq!(
            language_advisory(Some("zh"), Some("zf_xiaobei"), &kokoro),
            None
        );
        assert_eq!(
            language_advisory(Some("en"), Some("af_heart"), &kokoro),
            None
        );
    }

    #[test]
    fn the_preset_voice_language_outranks_the_engine_list() {
        let kokoro = langs(&KOKORO_DESKTOP_LANGUAGES);
        let mismatch = language_advisory(Some("zh"), Some("af_heart"), &kokoro).expect("mismatch");
        assert_eq!(mismatch.book, "zh");
        assert_eq!(mismatch.voice_speaks.as_deref(), Some("en"));
    }

    #[test]
    fn a_voice_without_a_preset_language_falls_back_to_the_engine_list() {
        let mismatch = language_advisory(Some("zh"), None, &langs(&["en"])).expect("mismatch");
        assert_eq!(mismatch.book, "zh");
        assert_eq!(mismatch.voice_speaks, None);
        assert_eq!(language_advisory(Some("en"), None, &langs(&["en"])), None);
    }

    #[test]
    fn an_unparseable_preset_id_falls_back_to_the_engine_list() {
        let mismatch = language_advisory(Some("zh"), Some("en_US-lessac-medium"), &langs(&["en"]))
            .expect("mismatch");
        assert_eq!(mismatch.voice_speaks, None);
    }

    #[test]
    fn an_engine_that_declares_no_languages_is_not_accused() {
        assert_eq!(language_advisory(Some("zh"), None, &[]), None);
    }

    #[test]
    fn region_subtags_and_case_never_split_a_language() {
        assert_eq!(
            language_advisory(Some("EN-us"), None, &langs(&["en"])),
            None
        );
        assert_eq!(
            language_advisory(Some("zh_CN"), None, &langs(&["ZH-Hans"])),
            None
        );
    }

    #[test]
    fn all_bundled_kokoro_voices_parse() {
        let catalog = crate::catalog::bundled_catalog();
        let entry = catalog
            .models
            .iter()
            .find(|e| e.id == "kokoro-82m")
            .expect("kokoro entry");
        let voices: Vec<&str> = entry
            .download
            .files
            .iter()
            .filter_map(|f| {
                f.dest_path
                    .strip_prefix("voices/")
                    .and_then(|rest| rest.strip_suffix(".pt"))
            })
            .collect();
        assert_eq!(voices.len(), 41);
        for voice in voices {
            let info = parse_voice_id(voice).expect(voice);
            assert!(
                KOKORO_DESKTOP_LANGUAGES.contains(&info.lang_code.as_str()),
                "{voice} needs a pronunciation model the desktop no longer ships"
            );
        }
        assert_eq!(entry.languages, KOKORO_DESKTOP_LANGUAGES);
    }

    #[test]
    fn kokoro_presets_outside_the_desktop_languages_are_not_listed() {
        for voice in ["af_heart", "bm_george", "jf_alpha", "zm_yunxi"] {
            assert!(is_listed_local_voice("kokoro-82m", Some(voice)), "{voice}");
        }
        for voice in ["ef_dora", "ff_siwis", "hm_psi", "if_sara", "pm_alex"] {
            assert!(!is_listed_local_voice("kokoro-82m", Some(voice)), "{voice}");
        }
        assert!(is_listed_local_voice("chatterbox-0.5b", None));
        assert!(is_listed_local_voice("kokoro-82m", None));
        assert!(!is_listed_local_voice("xtts-v2", Some("af_heart")));
        assert!(!is_listed_local_voice("cloud", None));
    }

    #[test]
    fn sample_sentences_cover_all_supported_languages() {
        for lang in [
            "en", "es", "fr", "de", "it", "pt", "pl", "tr", "ru", "nl", "cs", "ar", "zh", "ja",
            "hu", "ko", "hi",
        ] {
            let sentence = sample_sentence(lang);
            assert!(!sentence.is_empty(), "missing sentence for: {lang}");
            if lang != "en" {
                assert_ne!(
                    sentence,
                    sample_sentence("en"),
                    "language {lang} defaulted to English"
                );
            }
        }
    }
}
