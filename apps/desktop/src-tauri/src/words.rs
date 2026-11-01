use serde::{Deserialize, Serialize};

pub const WORDS_VERSION: u32 = 2;
pub const SOURCE_TTS_NATIVE: &str = "tts_native";
pub const SOURCE_WHISPERX: &str = "whisperx";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Timebase {
    Trimmed,
    #[serde(other)]
    #[default]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::Type)]
#[sqlx(transparent)]
pub struct WordsJson(String);

#[derive(Debug, Clone, Serialize)]
struct Envelope<'a> {
    v: u32,
    timebase: Timebase,
    words: &'a [serde_json::Value],
}

#[derive(Debug, Clone, Deserialize)]
struct StoredEnvelope {
    #[serde(default)]
    v: u32,
    #[serde(default)]
    timebase: Timebase,
    words: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WordsPayload {
    pub words: Vec<serde_json::Value>,
    pub timebase: Timebase,
    pub versioned: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WordProvenance {
    Fresh,
    Stale,
    Missing,
}

impl WordsJson {
    pub fn declare(
        words: &[serde_json::Value],
        timebase: Timebase,
    ) -> Result<Self, serde_json::Error> {
        let envelope = Envelope {
            v: WORDS_VERSION,
            timebase,
            words,
        };
        Ok(WordsJson(serde_json::to_string(&envelope)?))
    }

    pub fn from_stored(raw: String) -> Self {
        WordsJson(raw)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn parse(&self) -> WordsPayload {
        if let Ok(envelope) = serde_json::from_str::<StoredEnvelope>(&self.0) {
            let versioned = envelope.v == WORDS_VERSION;
            return WordsPayload {
                words: envelope.words,
                timebase: if versioned {
                    envelope.timebase
                } else {
                    Timebase::Unknown
                },
                versioned,
            };
        }
        let words = serde_json::from_str::<Vec<serde_json::Value>>(&self.0).unwrap_or_default();
        WordsPayload {
            words,
            timebase: Timebase::Unknown,
            versioned: false,
        }
    }
}

pub fn classify(payload: &WordsPayload, source: &str) -> WordProvenance {
    if payload.words.is_empty() {
        return WordProvenance::Missing;
    }
    if payload.versioned {
        return match payload.timebase {
            Timebase::Trimmed => WordProvenance::Fresh,
            Timebase::Unknown => WordProvenance::Stale,
        };
    }
    if source == SOURCE_WHISPERX {
        WordProvenance::Fresh
    } else {
        WordProvenance::Stale
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words() -> Vec<serde_json::Value> {
        vec![
            serde_json::json!({"w": "one", "s": 0, "e": 100}),
            serde_json::json!({"w": "two", "s": 100, "e": 250}),
        ]
    }

    #[test]
    fn a_declared_envelope_round_trips_its_timebase_and_words() {
        let stored = WordsJson::declare(&words(), Timebase::Trimmed).expect("declare");
        let payload = stored.parse();
        assert!(payload.versioned);
        assert_eq!(payload.timebase, Timebase::Trimmed);
        assert_eq!(payload.words, words());
        assert_eq!(classify(&payload, SOURCE_TTS_NATIVE), WordProvenance::Fresh);
    }

    #[test]
    fn every_row_shape_lands_in_exactly_one_provenance_class() {
        let declared_trimmed = WordsJson::declare(&words(), Timebase::Trimmed).expect("declare");
        let declared_unknown = WordsJson::declare(&words(), Timebase::Unknown).expect("declare");
        let legacy = WordsJson::from_stored(serde_json::to_string(&words()).expect("json"));
        let empty_declared = WordsJson::declare(&[], Timebase::Trimmed).expect("declare");
        let empty_legacy = WordsJson::from_stored("[]".into());

        let cases = [
            (&declared_trimmed, SOURCE_TTS_NATIVE, WordProvenance::Fresh),
            (&declared_trimmed, SOURCE_WHISPERX, WordProvenance::Fresh),
            (&declared_unknown, SOURCE_TTS_NATIVE, WordProvenance::Stale),
            (&declared_unknown, SOURCE_WHISPERX, WordProvenance::Stale),
            (&legacy, SOURCE_WHISPERX, WordProvenance::Fresh),
            (&legacy, SOURCE_TTS_NATIVE, WordProvenance::Stale),
            (&empty_declared, SOURCE_TTS_NATIVE, WordProvenance::Missing),
            (&empty_legacy, SOURCE_WHISPERX, WordProvenance::Missing),
        ];
        for (stored, source, expected) in cases {
            assert_eq!(
                classify(&stored.parse(), source),
                expected,
                "source {source} on {}",
                stored.as_str()
            );
        }
    }

    #[test]
    fn a_future_version_is_readable_but_never_reads_as_trimmed() {
        let future = WordsJson::from_stored(
            serde_json::json!({"v": 99, "timebase": "trimmed", "words": words()}).to_string(),
        );
        let payload = future.parse();
        assert!(!payload.versioned);
        assert_eq!(payload.timebase, Timebase::Unknown);
        assert_eq!(payload.words, words());
        assert_eq!(classify(&payload, SOURCE_TTS_NATIVE), WordProvenance::Stale);
    }

    #[test]
    fn an_unrecognised_timebase_string_reads_as_unknown() {
        let raw = WordsJson::from_stored(
            serde_json::json!({"v": WORDS_VERSION, "timebase": "raw", "words": words()})
                .to_string(),
        );
        assert_eq!(raw.parse().timebase, Timebase::Unknown);
        let absent = WordsJson::from_stored(
            serde_json::json!({"v": WORDS_VERSION, "words": words()}).to_string(),
        );
        assert_eq!(absent.parse().timebase, Timebase::Unknown);
    }

    #[test]
    fn an_undeclarable_timebase_is_never_written_as_trimmed() {
        let stored = WordsJson::declare(&words(), Timebase::Unknown).expect("declare");
        assert!(
            stored.as_str().contains("\"timebase\":\"unknown\""),
            "{}",
            stored.as_str()
        );
    }

    #[test]
    fn garbage_parses_to_missing_rather_than_panicking() {
        let payload = WordsJson::from_stored("{not json".into()).parse();
        assert!(payload.words.is_empty());
        assert_eq!(classify(&payload, SOURCE_WHISPERX), WordProvenance::Missing);
    }
}
