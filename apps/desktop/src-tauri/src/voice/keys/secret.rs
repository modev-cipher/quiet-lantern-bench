//! The only type allowed to hold BYOK key material (VOICE_AGENT §7,
//! ARCHITECTURE §8.1).
//!
//! Invariants enforced here and tested in this module and in `hygiene`:
//! - key bytes are zeroized on drop (defense-in-depth, not a guarantee —
//!   TLS/WS buffers copy);
//! - `Debug` and `Display` are redacting: no key bytes, not even the last 4,
//!   ever reach a formatter (the UI's `…last4` is an explicit, separate call);
//! - the type is deliberately not `Serialize`/`Deserialize`, so no key can
//!   ride a struct into SQLite, logs, or an export.

use std::fmt;

use zeroize::Zeroizing;

pub struct ApiKey(Zeroizing<String>);

impl ApiKey {
    pub fn new(raw: String) -> Self {
        Self(Zeroizing::new(raw))
    }

    pub fn last4(&self) -> String {
        let trimmed = self.0.trim();
        let tail: Vec<char> = trimmed.chars().rev().take(4).collect();
        tail.into_iter().rev().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.0.trim().is_empty()
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }

    /// Whether two keys hold the same material (ignoring the whitespace
    /// padding `last4`/`is_empty` already ignore). Comparison lives here so
    /// callers — the bulk-import duplicate check — never need `expose`; no
    /// bytes leave the type. Deliberately not `PartialEq`: an `==` would be
    /// too easy to reach for in a context that then formats the operands.
    pub(crate) fn matches(&self, other: &ApiKey) -> bool {
        self.0.trim() == other.0.trim()
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(redacted)")
    }
}

impl fmt::Display for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(redacted)")
    }
}

impl Clone for ApiKey {
    fn clone(&self) -> Self {
        Self(Zeroizing::new(self.0.as_str().to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "sk_car_supersecret_9f3a";

    #[derive(Debug)]
    #[allow(dead_code)]
    struct HoldsKey {
        base_url: String,
        key: ApiKey,
    }

    #[test]
    fn debug_of_a_key_holding_struct_leaks_no_key_bytes() {
        let holder = HoldsKey {
            base_url: "https://api.cartesia.ai".into(),
            key: ApiKey::new(KEY.into()),
        };
        let rendered = format!("{holder:?}");
        assert!(
            !rendered.contains("supersecret") && !rendered.contains("9f3a"),
            "a derived Debug on any struct holding an ApiKey must stay redacted, because \
             debug-formatting is exactly how keys historically end up in logs: {rendered}"
        );
        assert!(
            rendered.contains("redacted"),
            "the redaction marker is what proves the impl ran instead of printing nothing"
        );
    }

    #[test]
    fn display_is_redacted_too() {
        let rendered = format!("{}", ApiKey::new(KEY.into()));
        assert_eq!(
            rendered, "ApiKey(redacted)",
            "Display is the formatter error strings use, so it must redact exactly like Debug"
        );
    }

    #[test]
    fn last4_is_an_explicit_call_and_survives_whitespace() {
        assert_eq!(ApiKey::new("  abcd1234\n".into()).last4(), "1234");
        assert_eq!(
            ApiKey::new("xy".into()).last4(),
            "xy",
            "a key shorter than 4 chars must not panic; showing all of a 2-char string \
             reveals nothing a 4-char tail would not"
        );
    }

    #[test]
    fn emptiness_ignores_whitespace_padding() {
        assert!(ApiKey::new("   \n".into()).is_empty());
        assert!(!ApiKey::new(" k ".into()).is_empty());
    }
}
