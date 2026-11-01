//! Bulk-import parsing for `NAME=VALUE` key files (the owner's 47-key .env
//! case, RCA §9). This file is parsing only: it turns text into [`ApiKey`]s
//! and safe-to-render rejection reasons, and deliberately has no database
//! imports to misuse — the same structural rule `hygiene` pins on the other
//! secret-touching files. Values leave this module only inside [`ApiKey`];
//! every reason string is fixed text plus a line number, never derived from
//! the value, so nothing a caller can render carries key bytes.

use serde::Serialize;
use zeroize::Zeroize;

use super::secret::ApiKey;

/// One accepted line: the env-var name becomes the slot's label.
pub struct ParsedKey {
    pub label: String,
    pub key: ApiKey,
    pub line: usize,
}

/// One rejected line, by 1-based line number. `reason` is one of the fixed
/// strings below — a rejected line's *content* never rides along.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RejectedLine {
    pub line: usize,
    pub reason: &'static str,
}

pub const REASON_NOT_NAME_VALUE: &str = "not a NAME=VALUE line";
pub const REASON_BAD_NAME: &str = "the name before '=' is not a plain identifier";
pub const REASON_EMPTY_VALUE: &str = "the value is empty";
pub const REASON_VALUE_WHITESPACE: &str = "the value contains whitespace";

pub struct ParseOutcome {
    pub keys: Vec<ParsedKey>,
    pub rejected: Vec<RejectedLine>,
}

fn is_identifier(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Strips one matching pair of surrounding quotes — .env files commonly write
/// `NAME="value"` — without touching quotes inside the value.
fn unquoted(value: &str) -> &str {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 {
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        if first == last && (first == b'"' || first == b'\'') {
            return &value[1..value.len() - 1];
        }
    }
    value
}

/// Parses `NAME=VALUE` lines: blank lines and `#` comments are skipped
/// silently (they are structure, not mistakes); everything else either
/// yields a key or a line-numbered rejection. An `export ` prefix is
/// tolerated because shell-sourceable pool files carry one.
pub fn parse_env_lines(text: &str) -> ParseOutcome {
    let mut keys = Vec::new();
    let mut rejected = Vec::new();
    for (index, raw_line) in text.lines().enumerate() {
        let line = index + 1;
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let stripped = trimmed.strip_prefix("export ").unwrap_or(trimmed);
        let Some((name, value)) = stripped.split_once('=') else {
            rejected.push(RejectedLine {
                line,
                reason: REASON_NOT_NAME_VALUE,
            });
            continue;
        };
        let name = name.trim();
        if !is_identifier(name) {
            rejected.push(RejectedLine {
                line,
                reason: REASON_BAD_NAME,
            });
            continue;
        }
        let mut value = unquoted(value.trim()).to_owned();
        if value.is_empty() {
            rejected.push(RejectedLine {
                line,
                reason: REASON_EMPTY_VALUE,
            });
            continue;
        }
        if value.chars().any(char::is_whitespace) {
            value.zeroize();
            rejected.push(RejectedLine {
                line,
                reason: REASON_VALUE_WHITESPACE,
            });
            continue;
        }
        keys.push(ParsedKey {
            label: name.to_owned(),
            key: ApiKey::new(value),
            line,
        });
    }
    ParseOutcome { keys, rejected }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pool_file_shape_parses_names_as_labels_and_values_as_keys() {
        let text = "# gemini pool\n\nGEMINI_KEY_1=AIzaSecretOne1111\nexport GEMINI_KEY_2=\"AIzaSecretTwo2222\"\n";
        let outcome = parse_env_lines(text);
        assert!(outcome.rejected.is_empty());
        let labels: Vec<&str> = outcome.keys.iter().map(|k| k.label.as_str()).collect();
        assert_eq!(labels, vec!["GEMINI_KEY_1", "GEMINI_KEY_2"]);
        assert_eq!(outcome.keys[0].key.last4(), "1111");
        assert_eq!(
            outcome.keys[1].key.last4(),
            "2222",
            "surrounding quotes are .env syntax, not key bytes"
        );
        assert_eq!(outcome.keys[1].line, 4);
    }

    #[test]
    fn rejects_name_each_failure_by_line_and_fixed_reason_only() {
        let text = "just words\n=nokey\nBAD NAME=x\nEMPTY=\nSPACED=a b\nGOOD=k123";
        let outcome = parse_env_lines(text);
        assert_eq!(outcome.keys.len(), 1);
        assert_eq!(
            outcome.rejected,
            vec![
                RejectedLine {
                    line: 1,
                    reason: REASON_NOT_NAME_VALUE
                },
                RejectedLine {
                    line: 2,
                    reason: REASON_BAD_NAME
                },
                RejectedLine {
                    line: 3,
                    reason: REASON_BAD_NAME
                },
                RejectedLine {
                    line: 4,
                    reason: REASON_EMPTY_VALUE
                },
                RejectedLine {
                    line: 5,
                    reason: REASON_VALUE_WHITESPACE
                },
            ]
        );
        let serialized = serde_json::to_string(&outcome.rejected).unwrap();
        assert!(
            !serialized.contains("nokey") && !serialized.contains("a b"),
            "rejections cross IPC to the webview; line content must never ride along"
        );
    }

    #[test]
    fn comments_and_blanks_are_structure_not_mistakes() {
        let outcome = parse_env_lines("\n# a comment\n   \n\t\n");
        assert!(outcome.keys.is_empty());
        assert!(
            outcome.rejected.is_empty(),
            "a file of comments imports zero keys with zero complaints"
        );
    }

    #[test]
    fn a_byte_order_mark_rejects_the_line_it_rides_never_silently_minting_a_key() {
        // A UTF-8 BOM is not whitespace, so trim() keeps it: a BOM-prefixed
        // NAME fails the identifier check and a BOM-prefixed comment is no
        // longer a comment. Pinned as-built: both come back as line-numbered
        // rejections with fixed reasons — a Windows-saved pool file loses
        // its FIRST line loudly instead of importing a mangled key. (If BOM
        // stripping ever lands, re-pin this deliberately.)
        let bom = parse_env_lines("\u{feff}GEMINI_KEY_1=bom-key-aa11\nGEMINI_KEY_2=bom-key-bb22");
        assert_eq!(bom.keys.len(), 1, "only the BOM-free line imports");
        assert_eq!(bom.keys[0].label, "GEMINI_KEY_2");
        assert_eq!(
            bom.rejected,
            vec![RejectedLine {
                line: 1,
                reason: REASON_BAD_NAME
            }]
        );
        let bom_comment = parse_env_lines("\u{feff}# pool file\nK=bom-key-cc33");
        assert_eq!(bom_comment.keys.len(), 1);
        assert_eq!(
            bom_comment.rejected,
            vec![RejectedLine {
                line: 1,
                reason: REASON_NOT_NAME_VALUE
            }],
            "a BOM-prefixed comment is rejected, not silently skipped"
        );
    }

    #[test]
    fn crlf_line_endings_and_padding_whitespace_never_reach_the_key_bytes() {
        // Windows-saved files: lines() strips the \r with the \n, and
        // name/value trimming eats padding — none of it may survive into
        // the stored bytes, where an invisible \r would fail every dial.
        let outcome =
            parse_env_lines("A=crlf-key-aa11\r\nB  =  crlf-key-bb22  \r\nC\t=\tcrlf-key-cc33\r\n");
        assert!(outcome.rejected.is_empty(), "{:?}", outcome.rejected);
        assert_eq!(
            outcome
                .keys
                .iter()
                .map(|k| (k.label.as_str(), k.key.last4()))
                .collect::<Vec<_>>(),
            vec![
                ("A", "aa11".to_string()),
                ("B", "bb22".to_string()),
                ("C", "cc33".to_string())
            ],
            "no \\r, no padding space, no tab survives into label or key"
        );
    }

    #[test]
    fn everything_after_the_first_equals_is_value_verbatim() {
        // Keys can legitimately contain '=' (base64 padding); only the FIRST
        // '=' is structure. And quotes are stripped exactly once.
        let outcome = parse_env_lines("TOK=abc=de==\nQUOTED=\"'x=y'\"");
        assert!(outcome.rejected.is_empty());
        assert_eq!(outcome.keys[0].key.last4(), "de==");
        assert_eq!(
            outcome.keys[1].key.last4(),
            "x=y'",
            "one quote layer is .env syntax; the inner quotes are key bytes"
        );
    }

    #[test]
    fn a_lone_quote_or_mismatched_quotes_stay_part_of_the_value() {
        let outcome = parse_env_lines("A='half\nB=\"\"\nC='x\"");
        assert_eq!(outcome.keys[0].key.last4(), "half");
        assert_eq!(
            outcome.rejected,
            vec![RejectedLine {
                line: 2,
                reason: REASON_EMPTY_VALUE
            }],
            "an empty quoted value is an empty value"
        );
        assert_eq!(outcome.keys[1].key.last4(), "'x\"");
    }
}
