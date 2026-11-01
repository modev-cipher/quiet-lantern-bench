//! Ed25519 over domain-separated messages. The public half is compiled into the
//! app (base64, 32 bytes); the private half exists only on the owner's machine
//! and in one CI secret. Manifest bytes and artifact digests are signed under
//! different prefixes so a signature for one can never be replayed as the other.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use ring::signature::{self, Ed25519KeyPair, KeyPair, UnparsedPublicKey};

use super::error::UpdateError;

const MANIFEST_PREFIX: &[u8] = b"txtcaster-update-manifest-v1\n";
const ARTIFACT_PREFIX: &[u8] = b"txtcaster-update-artifact-v1\n";
const ENGINE_MANIFEST_PREFIX: &[u8] = b"txtcaster-engine-manifest-v1\n";
const ENGINE_ARCHIVE_PREFIX: &[u8] = b"txtcaster-engine-archive-v1\n";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicKey([u8; 32]);

impl PublicKey {
    pub fn from_base64(text: &str) -> Result<Self, String> {
        let bytes = B64
            .decode(text.trim())
            .map_err(|e| format!("public key is not base64: {e}"))?;
        let arr: [u8; 32] = bytes
            .try_into()
            .map_err(|_| "public key must be 32 bytes".to_string())?;
        Ok(PublicKey(arr))
    }

    pub fn to_base64(&self) -> String {
        B64.encode(self.0)
    }

    fn verify(&self, message: &[u8], signature_b64: &str) -> bool {
        let Ok(sig) = B64.decode(signature_b64.trim()) else {
            return false;
        };
        UnparsedPublicKey::new(&signature::ED25519, self.0)
            .verify(message, &sig)
            .is_ok()
    }

    /// Manifest envelopes carry the raw payload bytes; the signature is over
    /// those exact bytes under the manifest prefix.
    pub fn verify_manifest(&self, payload: &[u8], signature_b64: &str) -> Result<(), UpdateError> {
        if self.verify(&with_prefix(MANIFEST_PREFIX, payload), signature_b64) {
            Ok(())
        } else {
            Err(UpdateError::ManifestSignature)
        }
    }

    /// Artifacts are signed by their sha256 digest (32 raw bytes) so a 300 MB
    /// installer never has to be held in memory to be checked.
    pub fn verify_artifact_digest(
        &self,
        digest: &[u8; 32],
        signature_b64: &str,
    ) -> Result<(), UpdateError> {
        if self.verify(&with_prefix(ARTIFACT_PREFIX, digest), signature_b64) {
            Ok(())
        } else {
            Err(UpdateError::ArtifactSignature)
        }
    }

    pub fn verify_engine_manifest(
        &self,
        payload: &[u8],
        signature_b64: &str,
    ) -> Result<(), UpdateError> {
        if self.verify(&with_prefix(ENGINE_MANIFEST_PREFIX, payload), signature_b64) {
            Ok(())
        } else {
            Err(UpdateError::ManifestSignature)
        }
    }

    pub fn verify_engine_archive_digest(
        &self,
        digest: &[u8; 32],
        signature_b64: &str,
    ) -> Result<(), UpdateError> {
        if self.verify(&with_prefix(ENGINE_ARCHIVE_PREFIX, digest), signature_b64) {
            Ok(())
        } else {
            Err(UpdateError::ArtifactSignature)
        }
    }
}

pub struct SigningKey(Ed25519KeyPair);

impl SigningKey {
    /// Fresh keypair; returns the PKCS#8 v2 document (the only form the private
    /// half is ever stored in) alongside the key.
    pub fn generate() -> Result<(Vec<u8>, Self), String> {
        let rng = ring::rand::SystemRandom::new();
        let doc = Ed25519KeyPair::generate_pkcs8(&rng).map_err(|e| e.to_string())?;
        let key = Ed25519KeyPair::from_pkcs8(doc.as_ref()).map_err(|e| e.to_string())?;
        Ok((doc.as_ref().to_vec(), SigningKey(key)))
    }

    pub fn from_pkcs8(bytes: &[u8]) -> Result<Self, String> {
        Ed25519KeyPair::from_pkcs8(bytes)
            .map(SigningKey)
            .map_err(|e| format!("private key rejected: {e}"))
    }

    pub fn public_key(&self) -> PublicKey {
        let mut arr = [0u8; 32];
        arr.copy_from_slice(self.0.public_key().as_ref());
        PublicKey(arr)
    }

    pub fn sign_manifest(&self, payload: &[u8]) -> String {
        B64.encode(self.0.sign(&with_prefix(MANIFEST_PREFIX, payload)).as_ref())
    }

    pub fn sign_artifact_digest(&self, digest: &[u8; 32]) -> String {
        B64.encode(self.0.sign(&with_prefix(ARTIFACT_PREFIX, digest)).as_ref())
    }

    pub fn sign_engine_manifest(&self, payload: &[u8]) -> String {
        B64.encode(
            self.0
                .sign(&with_prefix(ENGINE_MANIFEST_PREFIX, payload))
                .as_ref(),
        )
    }

    pub fn sign_engine_archive_digest(&self, digest: &[u8; 32]) -> String {
        B64.encode(
            self.0
                .sign(&with_prefix(ENGINE_ARCHIVE_PREFIX, digest))
                .as_ref(),
        )
    }
}

fn with_prefix(prefix: &[u8], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(prefix.len() + body.len());
    out.extend_from_slice(prefix);
    out.extend_from_slice(body);
    out
}
