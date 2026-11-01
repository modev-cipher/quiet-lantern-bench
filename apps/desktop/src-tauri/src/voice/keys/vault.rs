//! Passphrase-encrypted key file — the first Linux fallback when no Secret
//! Service is available (VOICE_AGENT §7): Argon2id (memory-hard params pinned
//! below), random per-write salt, unique nonce per write (a fresh salt derives
//! a fresh key, so nonce uniqueness holds per key trivially), ChaCha20-Poly1305
//! with the whole header authenticated as AAD, file mode 0600, atomic
//! write-then-rename, plaintext buffers zeroized.

use std::collections::BTreeMap;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use rand::RngCore;
use zeroize::Zeroizing;

use super::secret::ApiKey;

const MAGIC: &[u8; 8] = b"TXTCVLT1";
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const HEADER_LEN: usize = 8 + 4 + 4 + 4 + SALT_LEN + NONCE_LEN;

pub const ARGON2_M_COST_KIB: u32 = 65536;
pub const ARGON2_T_COST: u32 = 3;
pub const ARGON2_P_COST: u32 = 1;

const MAX_M_COST_KIB: u32 = 262144;
const MAX_T_COST: u32 = 16;
const MAX_P_COST: u32 = 8;

#[derive(Debug)]
pub enum VaultError {
    Io(std::io::ErrorKind),
    Malformed,
    UnsafeParams,
    WrongPassphraseOrCorrupt,
    Kdf,
}

impl fmt::Display for VaultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VaultError::Io(kind) => write!(f, "vault file I/O failed: {kind}"),
            VaultError::Malformed => f.write_str("vault file is not a txtCaster key vault"),
            VaultError::UnsafeParams => {
                f.write_str("vault header carries out-of-range KDF parameters")
            }
            VaultError::WrongPassphraseOrCorrupt => {
                f.write_str("wrong passphrase, or the vault file was modified")
            }
            VaultError::Kdf => f.write_str("passphrase key derivation failed"),
        }
    }
}

impl From<std::io::Error> for VaultError {
    fn from(err: std::io::Error) -> Self {
        VaultError::Io(err.kind())
    }
}

pub struct FileVault {
    path: PathBuf,
}

impl FileVault {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn exists(&self) -> bool {
        self.path.is_file()
    }

    pub fn read_all(&self, passphrase: &str) -> Result<BTreeMap<String, ApiKey>, VaultError> {
        let bytes = std::fs::read(&self.path)?;
        if bytes.len() <= HEADER_LEN || &bytes[..8] != MAGIC {
            return Err(VaultError::Malformed);
        }
        let m_cost = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        let t_cost = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
        let p_cost = u32::from_le_bytes(bytes[16..20].try_into().unwrap());
        if m_cost == 0
            || t_cost == 0
            || p_cost == 0
            || m_cost > MAX_M_COST_KIB
            || t_cost > MAX_T_COST
            || p_cost > MAX_P_COST
        {
            return Err(VaultError::UnsafeParams);
        }
        let salt = &bytes[20..20 + SALT_LEN];
        let nonce = &bytes[20 + SALT_LEN..HEADER_LEN];
        let cipher_key = derive_key(passphrase, salt, m_cost, t_cost, p_cost)?;
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&*cipher_key));
        let plaintext = Zeroizing::new(
            cipher
                .decrypt(
                    Nonce::from_slice(nonce),
                    Payload {
                        msg: &bytes[HEADER_LEN..],
                        aad: &bytes[..HEADER_LEN],
                    },
                )
                .map_err(|_| VaultError::WrongPassphraseOrCorrupt)?,
        );
        let entries: BTreeMap<String, String> =
            serde_json::from_slice(&plaintext).map_err(|_| VaultError::WrongPassphraseOrCorrupt)?;
        Ok(entries
            .into_iter()
            .map(|(url, key)| (url, ApiKey::new(key)))
            .collect())
    }

    pub fn write_all(
        &self,
        passphrase: &str,
        entries: &BTreeMap<String, ApiKey>,
    ) -> Result<(), VaultError> {
        let mut salt = [0u8; SALT_LEN];
        rand::rng().fill_bytes(&mut salt);
        let mut nonce = [0u8; NONCE_LEN];
        rand::rng().fill_bytes(&mut nonce);

        let mut header = Vec::with_capacity(HEADER_LEN);
        header.extend_from_slice(MAGIC);
        header.extend_from_slice(&ARGON2_M_COST_KIB.to_le_bytes());
        header.extend_from_slice(&ARGON2_T_COST.to_le_bytes());
        header.extend_from_slice(&ARGON2_P_COST.to_le_bytes());
        header.extend_from_slice(&salt);
        header.extend_from_slice(&nonce);

        let exposed: BTreeMap<&str, &str> = entries
            .iter()
            .map(|(url, key)| (url.as_str(), key.expose()))
            .collect();
        let plaintext =
            Zeroizing::new(serde_json::to_vec(&exposed).map_err(|_| VaultError::Malformed)?);

        let cipher_key = derive_key(
            passphrase,
            &salt,
            ARGON2_M_COST_KIB,
            ARGON2_T_COST,
            ARGON2_P_COST,
        )?;
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&*cipher_key));
        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &plaintext,
                    aad: &header,
                },
            )
            .map_err(|_| VaultError::Kdf)?;

        let parent = self.path.parent().ok_or(VaultError::Malformed)?;
        std::fs::create_dir_all(parent)?;
        let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tmp.as_file()
                .set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        tmp.write_all(&header)?;
        tmp.write_all(&ciphertext)?;
        tmp.as_file().sync_all()?;
        tmp.persist(&self.path).map_err(|err| err.error)?;
        Ok(())
    }
}

fn derive_key(
    passphrase: &str,
    salt: &[u8],
    m_cost: u32,
    t_cost: u32,
    p_cost: u32,
) -> Result<Zeroizing<[u8; 32]>, VaultError> {
    let params =
        Params::new(m_cost, t_cost, p_cost, Some(32)).map_err(|_| VaultError::UnsafeParams)?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = Zeroizing::new([0u8; 32]);
    argon
        .hash_password_into(passphrase.as_bytes(), salt, &mut *key)
        .map_err(|_| VaultError::Kdf)?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "dg_secret_key_material_77aa";

    fn sample_entries() -> BTreeMap<String, ApiKey> {
        BTreeMap::from([
            (
                "https://api.deepgram.com".to_string(),
                ApiKey::new(SECRET.into()),
            ),
            (
                "https://api.cartesia.ai".to_string(),
                ApiKey::new("sk_car_other_key_11bb".into()),
            ),
        ])
    }

    #[test]
    fn roundtrip_restores_every_entry() {
        let dir = tempfile::tempdir().unwrap();
        let vault = FileVault::new(dir.path().join("keys.vault"));
        vault.write_all("correct horse", &sample_entries()).unwrap();
        let read = vault.read_all("correct horse").unwrap();
        assert_eq!(read.len(), 2);
        assert_eq!(read["https://api.deepgram.com"].expose(), SECRET);
    }

    #[test]
    fn the_file_at_rest_contains_no_key_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let vault = FileVault::new(dir.path().join("keys.vault"));
        vault.write_all("pw", &sample_entries()).unwrap();
        let raw = std::fs::read(vault.path()).unwrap();
        let haystack = String::from_utf8_lossy(&raw);
        assert!(
            !haystack.contains("dg_secret") && !haystack.contains("deepgram"),
            "an unencrypted or partially encrypted vault would defeat the whole fallback"
        );
    }

    #[test]
    fn a_wrong_passphrase_is_rejected_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let vault = FileVault::new(dir.path().join("keys.vault"));
        vault.write_all("right", &sample_entries()).unwrap();
        assert!(matches!(
            vault.read_all("wrong"),
            Err(VaultError::WrongPassphraseOrCorrupt)
        ));
    }

    #[test]
    fn flipping_a_ciphertext_byte_fails_authentication() {
        let dir = tempfile::tempdir().unwrap();
        let vault = FileVault::new(dir.path().join("keys.vault"));
        vault.write_all("pw", &sample_entries()).unwrap();
        let mut raw = std::fs::read(vault.path()).unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 0x01;
        std::fs::write(vault.path(), &raw).unwrap();
        assert!(matches!(
            vault.read_all("pw"),
            Err(VaultError::WrongPassphraseOrCorrupt)
        ));
    }

    #[test]
    fn tampering_with_the_authenticated_header_fails_even_when_params_stay_sane() {
        let dir = tempfile::tempdir().unwrap();
        let vault = FileVault::new(dir.path().join("keys.vault"));
        vault.write_all("pw", &sample_entries()).unwrap();
        let mut raw = std::fs::read(vault.path()).unwrap();
        raw[12] = 2;
        std::fs::write(vault.path(), &raw).unwrap();
        assert!(
            matches!(
                vault.read_all("pw"),
                Err(VaultError::WrongPassphraseOrCorrupt)
            ),
            "t_cost=2 is in range, so only AAD coverage of the header catches this edit — \
             without it an attacker could silently weaken the KDF parameters"
        );
    }

    #[test]
    fn absurd_kdf_params_are_rejected_before_any_derivation_runs() {
        let dir = tempfile::tempdir().unwrap();
        let vault = FileVault::new(dir.path().join("keys.vault"));
        vault.write_all("pw", &sample_entries()).unwrap();
        let mut raw = std::fs::read(vault.path()).unwrap();
        raw[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        std::fs::write(vault.path(), &raw).unwrap();
        assert!(
            matches!(vault.read_all("pw"), Err(VaultError::UnsafeParams)),
            "deriving with an attacker-chosen 4 TiB m_cost would OOM the app before \
             authentication ever ran"
        );
    }

    #[test]
    fn every_write_uses_a_fresh_salt_and_nonce() {
        let dir = tempfile::tempdir().unwrap();
        let vault = FileVault::new(dir.path().join("keys.vault"));
        vault.write_all("pw", &sample_entries()).unwrap();
        let first = std::fs::read(vault.path()).unwrap();
        vault.write_all("pw", &sample_entries()).unwrap();
        let second = std::fs::read(vault.path()).unwrap();
        assert_ne!(
            &first[20..20 + SALT_LEN],
            &second[20..20 + SALT_LEN],
            "salt reuse would derive the same key twice and hand nonce-uniqueness to luck"
        );
        assert_ne!(
            &first[20 + SALT_LEN..HEADER_LEN],
            &second[20 + SALT_LEN..HEADER_LEN]
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_vault_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let vault = FileVault::new(dir.path().join("keys.vault"));
        vault.write_all("pw", &sample_entries()).unwrap();
        let mode = std::fs::metadata(vault.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o600,
            "group/world-readable key files defeat the encryption for local attackers who \
             can also read the passphrase over the user's shoulder later"
        );
    }

    #[test]
    fn a_file_that_is_not_a_vault_is_reported_as_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.vault");
        std::fs::write(&path, b"not a vault").unwrap();
        assert!(matches!(
            FileVault::new(path).read_all("pw"),
            Err(VaultError::Malformed)
        ));
    }
}
