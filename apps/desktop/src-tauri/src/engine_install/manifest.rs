use std::collections::BTreeMap;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::updater::error::UpdateError;
use crate::updater::manifest::{url_is_acceptable, Envelope, MANIFEST_MAX_BYTES};
use crate::updater::signing::{PublicKey, SigningKey};

pub const LITE_VARIANT: &str = "lite";
pub const CPU_VARIANT: &str = "cpu";
pub const CUDA13_VARIANT: &str = "cuda13";

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct EnginePart {
    pub url: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct EngineArchive {
    pub size: u64,
    pub sha256: String,
    pub signature: String,
    pub unpacked_size: u64,
    pub parts: Vec<EnginePart>,
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct EngineManifest {
    pub engine_version: String,
    pub engine_api: u32,
    pub platforms: BTreeMap<String, BTreeMap<String, EngineArchive>>,
}

impl EngineManifest {
    pub fn archive(&self, platform: &str, variant: &str) -> Option<&EngineArchive> {
        self.platforms.get(platform)?.get(variant)
    }
}

pub fn sign_engine_manifest(manifest: &EngineManifest, key: &SigningKey) -> Envelope {
    let payload = serde_json::to_vec(manifest).expect("engine manifest serializes");
    Envelope {
        payload: B64.encode(&payload),
        signature: key.sign_engine_manifest(&payload),
    }
}

pub fn verify_and_parse_engine(
    envelope_bytes: &[u8],
    key: &PublicKey,
) -> Result<EngineManifest, UpdateError> {
    if envelope_bytes.len() > MANIFEST_MAX_BYTES {
        return Err(UpdateError::ManifestTooLarge {
            bytes: envelope_bytes.len(),
            cap: MANIFEST_MAX_BYTES,
        });
    }
    let envelope: Envelope =
        serde_json::from_slice(envelope_bytes).map_err(|_| UpdateError::ManifestSignature)?;
    let payload = B64
        .decode(envelope.payload.trim())
        .map_err(|_| UpdateError::ManifestSignature)?;
    key.verify_engine_manifest(&payload, &envelope.signature)?;
    let manifest: EngineManifest = serde_json::from_slice(&payload)
        .map_err(|e| UpdateError::ManifestMalformed(e.to_string()))?;
    semver::Version::parse(&manifest.engine_version)
        .map_err(|_| UpdateError::BadVersion(manifest.engine_version.clone()))?;
    for (platform, variants) in &manifest.platforms {
        for (variant, archive) in variants {
            check_archive(archive).map_err(|why| {
                UpdateError::ManifestMalformed(format!("{platform}/{variant}: {why}"))
            })?;
        }
    }
    Ok(manifest)
}

fn check_archive(archive: &EngineArchive) -> Result<(), String> {
    if archive.parts.is_empty() {
        return Err("no parts".into());
    }
    if !is_sha256_hex(&archive.sha256) {
        return Err("archive sha256 is not 32 hex bytes".into());
    }
    let mut total: u64 = 0;
    for part in &archive.parts {
        if !is_sha256_hex(&part.sha256) {
            return Err("part sha256 is not 32 hex bytes".into());
        }
        let url = reqwest::Url::parse(&part.url).map_err(|_| "part url does not parse")?;
        if !url_is_acceptable(&url) {
            return Err(format!("part url {} is not https", part.url));
        }
        total = total.checked_add(part.size).ok_or("part sizes overflow")?;
    }
    if total != archive.size {
        return Err(format!(
            "parts add up to {total} bytes, archive says {}",
            archive.size
        ));
    }
    Ok(())
}

fn is_sha256_hex(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::updater::manifest::verify_and_parse;

    fn key() -> SigningKey {
        SigningKey::generate().expect("keygen").1
    }

    fn part(url: &str, size: u64) -> EnginePart {
        EnginePart {
            url: url.to_string(),
            size,
            sha256: "ab".repeat(32),
        }
    }

    fn manifest(key: &SigningKey, parts: Vec<EnginePart>) -> EngineManifest {
        let size = parts.iter().map(|p| p.size).sum();
        let digest = [7u8; 32];
        let archive = EngineArchive {
            size,
            sha256: hex::encode(digest),
            signature: key.sign_engine_archive_digest(&digest),
            unpacked_size: 3 * size,
            parts,
        };
        EngineManifest {
            engine_version: "0.1.0".into(),
            engine_api: 1,
            platforms: BTreeMap::from([(
                "linux-x86_64".to_string(),
                BTreeMap::from([(CPU_VARIANT.to_string(), archive)]),
            )]),
        }
    }

    fn bytes(envelope: &Envelope) -> Vec<u8> {
        serde_json::to_vec(envelope).expect("json")
    }

    #[test]
    fn a_signed_engine_manifest_round_trips() {
        let key = key();
        let original = manifest(
            &key,
            vec![
                part("https://github.com/a/b/releases/download/v1/e.000", 10),
                part("https://github.com/a/b/releases/download/v1/e.001", 4),
            ],
        );
        let parsed = verify_and_parse_engine(
            &bytes(&sign_engine_manifest(&original, &key)),
            &key.public_key(),
        )
        .expect("verifies");
        assert_eq!(parsed, original);
        let archive = parsed
            .archive("linux-x86_64", CPU_VARIANT)
            .expect("cpu archive");
        assert!(key
            .public_key()
            .verify_engine_archive_digest(&[7u8; 32], &archive.signature)
            .is_ok());
    }

    #[test]
    fn an_engine_manifest_signed_by_another_key_is_refused() {
        let signer = key();
        let other = key();
        let envelope = sign_engine_manifest(
            &manifest(&signer, vec![part("https://example.org/e.000", 1)]),
            &signer,
        );
        assert_eq!(
            verify_and_parse_engine(&bytes(&envelope), &other.public_key()),
            Err(UpdateError::ManifestSignature)
        );
    }

    #[test]
    fn app_and_engine_signatures_cannot_stand_in_for_each_other() {
        let key = key();
        let payload =
            serde_json::to_vec(&manifest(&key, vec![part("https://example.org/e.000", 1)]))
                .expect("json");
        let as_app = Envelope {
            payload: B64.encode(&payload),
            signature: key.sign_manifest(&payload),
        };
        assert_eq!(
            verify_and_parse_engine(&bytes(&as_app), &key.public_key()),
            Err(UpdateError::ManifestSignature)
        );
        let engine_envelope = sign_engine_manifest(
            &manifest(&key, vec![part("https://example.org/e.000", 1)]),
            &key,
        );
        assert_eq!(
            verify_and_parse(&bytes(&engine_envelope), &key.public_key()).err(),
            Some(UpdateError::ManifestSignature)
        );
        let digest = [9u8; 32];
        assert!(key
            .public_key()
            .verify_engine_archive_digest(&digest, &key.sign_artifact_digest(&digest))
            .is_err());
        assert!(key
            .public_key()
            .verify_artifact_digest(&digest, &key.sign_engine_archive_digest(&digest))
            .is_err());
    }

    #[test]
    fn parts_that_do_not_add_up_to_the_archive_are_refused() {
        let key = key();
        let mut bad = manifest(&key, vec![part("https://example.org/e.000", 5)]);
        if let Some(archive) = bad
            .platforms
            .get_mut("linux-x86_64")
            .and_then(|v| v.get_mut(CPU_VARIANT))
        {
            archive.size = 6;
        }
        let result =
            verify_and_parse_engine(&bytes(&sign_engine_manifest(&bad, &key)), &key.public_key());
        assert!(
            matches!(result, Err(UpdateError::ManifestMalformed(_))),
            "{result:?}"
        );
    }

    #[test]
    fn a_plaintext_part_url_is_refused() {
        let key = key();
        let bad = manifest(&key, vec![part("http://example.org/e.000", 5)]);
        let result =
            verify_and_parse_engine(&bytes(&sign_engine_manifest(&bad, &key)), &key.public_key());
        assert!(
            matches!(result, Err(UpdateError::ManifestMalformed(_))),
            "{result:?}"
        );
    }

    #[test]
    fn an_archive_with_no_parts_is_refused() {
        let key = key();
        let bad = manifest(&key, vec![]);
        let result =
            verify_and_parse_engine(&bytes(&sign_engine_manifest(&bad, &key)), &key.public_key());
        assert!(
            matches!(result, Err(UpdateError::ManifestMalformed(_))),
            "{result:?}"
        );
    }
}
