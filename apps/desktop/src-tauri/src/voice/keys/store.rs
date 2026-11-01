//! Key storage with the ranked fallback chain (VOICE_AGENT §7, ARCHITECTURE
//! §8.1): OS keychain via `keyring` first; where no Secret Service answers
//! (headless Linux), a passphrase-encrypted file (`vault`); if the user
//! declines a passphrase, session-only keys that die with the process.
//! Plaintext-with-warning is forbidden, so there is no fourth level.
//!
//! Keys are stored per base URL — the keyring account *is* the canonical base
//! URL — so a custom endpoint can never receive a named provider's key. The
//! active level is surfaced to the UI verbatim (`StorageStatus`), never
//! silently absorbed. All calls block; callers hop through `spawn_blocking`.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::Serialize;
use zeroize::Zeroizing;

use super::secret::ApiKey;
use super::vault::{FileVault, VaultError};

const KEYRING_SERVICE: &str = "txtCaster-voice-keys";
const KEYRING_PROBE_ACCOUNT: &str = "__availability-probe__";

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageLevel {
    OsKeychain,
    EncryptedFile,
    SessionOnly,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
pub struct StorageStatus {
    pub level: StorageLevel,
    pub locked: bool,
    pub fallback_has_entries: bool,
}

#[derive(Debug)]
pub enum KeyStoreError {
    VaultLocked,
    KeychainUnavailable,
    KeychainPresent,
    Keychain(String),
    Vault(VaultError),
}

impl fmt::Display for KeyStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyStoreError::VaultLocked => {
                f.write_str("the key vault is locked; unlock it with your passphrase first")
            }
            KeyStoreError::KeychainUnavailable => {
                f.write_str("no OS keychain is available on this system")
            }
            KeyStoreError::KeychainPresent => f.write_str(
                "the OS keychain is available, so lower-ranked storage cannot be selected",
            ),
            KeyStoreError::Keychain(detail) => write!(f, "OS keychain operation failed: {detail}"),
            KeyStoreError::Vault(err) => write!(f, "key vault operation failed: {err}"),
        }
    }
}

impl From<VaultError> for KeyStoreError {
    fn from(err: VaultError) -> Self {
        KeyStoreError::Vault(err)
    }
}

struct UnlockedVault {
    vault: FileVault,
    passphrase: Zeroizing<String>,
}

pub(crate) enum Backend {
    Keyring,
    FileLocked(FileVault),
    FileUnlocked {
        vault: FileVault,
        passphrase: Zeroizing<String>,
    },
    Session(BTreeMap<String, ApiKey>),
}

pub(crate) fn backend_for(keyring_available: bool, vault_path: PathBuf) -> Backend {
    if keyring_available {
        Backend::Keyring
    } else {
        Backend::FileLocked(FileVault::new(vault_path))
    }
}

pub struct KeyStore {
    vault_path: PathBuf,
    backend: Mutex<Option<Backend>>,
    fallback_vault: Mutex<Option<UnlockedVault>>,
}

impl KeyStore {
    pub fn new(vault_path: PathBuf) -> Self {
        Self {
            vault_path,
            backend: Mutex::new(None),
            fallback_vault: Mutex::new(None),
        }
    }

    /// Crate-internal: unit tests and the integration-proof constructor
    /// (`KeyManager::session_only_for_tests`) force a backend directly.
    pub(crate) fn with_forced_backend(vault_path: PathBuf, backend: Backend) -> Self {
        Self {
            vault_path,
            backend: Mutex::new(Some(backend)),
            fallback_vault: Mutex::new(None),
        }
    }

    fn with_backend<R>(&self, f: impl FnOnce(&mut Backend) -> R) -> R {
        let mut guard = self.backend.lock().expect("key store lock poisoned");
        let backend = guard.get_or_insert_with(|| {
            harden_process_memory();
            backend_for(keyring_roundtrip_works(), self.vault_path.clone())
        });
        f(backend)
    }

    pub fn status(&self) -> StorageStatus {
        self.with_backend(|backend| match backend {
            Backend::Keyring => {
                let fallback_has_entries =
                    if let Some(fb) = self.fallback_vault.lock().expect("fallback lock").as_ref() {
                        fb.vault.exists()
                            && fb
                                .vault
                                .read_all(&fb.passphrase)
                                .map(|entries| !entries.is_empty())
                                .unwrap_or(false)
                    } else {
                        self.vault_path.exists()
                    };
                StorageStatus {
                    level: StorageLevel::OsKeychain,
                    locked: false,
                    fallback_has_entries,
                }
            }
            Backend::FileLocked(_) => StorageStatus {
                level: StorageLevel::EncryptedFile,
                locked: true,
                fallback_has_entries: false,
            },
            Backend::FileUnlocked { .. } => StorageStatus {
                level: StorageLevel::EncryptedFile,
                locked: false,
                fallback_has_entries: false,
            },
            Backend::Session(_) => StorageStatus {
                level: StorageLevel::SessionOnly,
                locked: false,
                fallback_has_entries: false,
            },
        })
    }

    pub fn unlock_file_vault(&self, passphrase: &str) -> Result<(), KeyStoreError> {
        let is_keyring = self.with_backend(|backend| matches!(backend, Backend::Keyring));
        if is_keyring {
            let vault = FileVault::new(self.vault_path.clone());
            if vault.exists() {
                vault.read_all(passphrase)?;
            } else {
                vault.write_all(passphrase, &BTreeMap::new())?;
            }
            let mut fallback_guard = self.fallback_vault.lock().expect("fallback lock poisoned");
            *fallback_guard = Some(UnlockedVault {
                vault,
                passphrase: Zeroizing::new(passphrase.to_owned()),
            });
            return Ok(());
        }

        self.with_backend(|backend| {
            let vault = match backend {
                Backend::Keyring => unreachable!(),
                Backend::FileLocked(vault) => FileVault::new(vault.path().to_path_buf()),
                Backend::FileUnlocked { vault, .. } => FileVault::new(vault.path().to_path_buf()),
                Backend::Session(_) => FileVault::new(self.vault_path.clone()),
            };
            if vault.exists() {
                vault.read_all(passphrase)?;
            } else {
                vault.write_all(passphrase, &BTreeMap::new())?;
            }
            *backend = Backend::FileUnlocked {
                vault,
                passphrase: Zeroizing::new(passphrase.to_owned()),
            };
            Ok(())
        })
    }

    pub fn use_session_only(&self) -> Result<(), KeyStoreError> {
        self.with_backend(|backend| match backend {
            Backend::Keyring => Err(KeyStoreError::KeychainPresent),
            _ => {
                *backend = Backend::Session(BTreeMap::new());
                Ok(())
            }
        })
    }

    pub fn store(&self, base_url: &str, key: &ApiKey) -> Result<(), KeyStoreError> {
        self.with_backend(|backend| match backend {
            Backend::Keyring => keyring_entry(base_url)?
                .set_password(key.expose())
                .map_err(|err| KeyStoreError::Keychain(err.to_string())),
            Backend::FileLocked(_) => Err(KeyStoreError::VaultLocked),
            Backend::FileUnlocked { vault, passphrase } => {
                let mut entries = if vault.exists() {
                    vault.read_all(passphrase)?
                } else {
                    BTreeMap::new()
                };
                entries.insert(base_url.to_owned(), key.clone());
                Ok(vault.write_all(passphrase, &entries)?)
            }
            Backend::Session(entries) => {
                entries.insert(base_url.to_owned(), key.clone());
                Ok(())
            }
        })
    }

    pub fn load(&self, base_url: &str) -> Result<Option<ApiKey>, KeyStoreError> {
        let primary_res = self.with_backend(|backend| match backend {
            Backend::Keyring => match keyring_entry(base_url)?.get_password() {
                Ok(secret) => Ok(Some(ApiKey::new(secret))),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(err) => Err(KeyStoreError::Keychain(err.to_string())),
            },
            Backend::FileLocked(_) => Err(KeyStoreError::VaultLocked),
            Backend::FileUnlocked { vault, passphrase } => {
                if !vault.exists() {
                    return Ok(None);
                }
                Ok(vault.read_all(passphrase)?.remove(base_url))
            }
            Backend::Session(entries) => Ok(entries.get(base_url).cloned()),
        });

        match primary_res {
            Ok(Some(key)) => Ok(Some(key)),
            Ok(None) | Err(KeyStoreError::Keychain(_)) => {
                if let Some(fallback) = self.fallback_vault.lock().expect("fallback lock").as_ref()
                {
                    if fallback.vault.exists() {
                        if let Ok(entries) = fallback.vault.read_all(&fallback.passphrase) {
                            if let Some(key) = entries.get(base_url) {
                                return Ok(Some(key.clone()));
                            }
                        }
                    }
                }
                primary_res
            }
            Err(err) => Err(err),
        }
    }

    pub fn remove(&self, base_url: &str) -> Result<(), KeyStoreError> {
        let res = self.with_backend(|backend| match backend {
            Backend::Keyring => match keyring_entry(base_url)?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(err) => Err(KeyStoreError::Keychain(err.to_string())),
            },
            Backend::FileLocked(_) => Err(KeyStoreError::VaultLocked),
            Backend::FileUnlocked { vault, passphrase } => {
                if !vault.exists() {
                    return Ok(());
                }
                let mut entries = vault.read_all(passphrase)?;
                if entries.remove(base_url).is_some() {
                    vault.write_all(passphrase, &entries)?;
                }
                Ok(())
            }
            Backend::Session(entries) => {
                entries.remove(base_url);
                Ok(())
            }
        });

        if res.is_ok() {
            if let Some(fallback) = self.fallback_vault.lock().expect("fallback lock").as_mut() {
                if fallback.vault.exists() {
                    if let Ok(mut entries) = fallback.vault.read_all(&fallback.passphrase) {
                        if entries.remove(base_url).is_some() {
                            let _ = fallback.vault.write_all(&fallback.passphrase, &entries);
                        }
                    }
                }
            }
        }

        res
    }
}

fn keyring_entry(base_url: &str) -> Result<keyring::Entry, KeyStoreError> {
    keyring::Entry::new(KEYRING_SERVICE, base_url)
        .map_err(|err| KeyStoreError::Keychain(err.to_string()))
}

fn keyring_roundtrip_works() -> bool {
    let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, KEYRING_PROBE_ACCOUNT) else {
        return false;
    };
    if entry.set_password("probe").is_err() {
        return false;
    }
    let read_back = matches!(entry.get_password().as_deref(), Ok("probe"));
    let _ = entry.delete_credential();
    read_back
}

fn harden_process_memory() {
    #[cfg(target_os = "linux")]
    unsafe {
        libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "sk-proj-fallback_secret_42dd";
    const URL: &str = "https://api.example-llm.dev/v1";

    fn locked_store(dir: &tempfile::TempDir) -> KeyStore {
        let path = dir.path().join("keys.vault");
        KeyStore::with_forced_backend(path.clone(), Backend::FileLocked(FileVault::new(path)))
    }

    #[test]
    fn the_fallback_chain_prefers_the_keychain_and_falls_to_the_file_vault() {
        assert!(matches!(
            backend_for(true, PathBuf::from("/tmp/v")),
            Backend::Keyring
        ));
        assert!(matches!(
            backend_for(false, PathBuf::from("/tmp/v")),
            Backend::FileLocked(_)
        ));
    }

    #[test]
    fn a_locked_vault_refuses_every_key_operation_and_says_so_in_its_status() {
        let dir = tempfile::tempdir().unwrap();
        let store = locked_store(&dir);
        assert_eq!(
            store.status(),
            StorageStatus {
                level: StorageLevel::EncryptedFile,
                locked: true,
                fallback_has_entries: false,
            },
            "the UI can only surface the fallback level if the store reports it honestly"
        );
        assert!(matches!(
            store.store(URL, &ApiKey::new(KEY.into())),
            Err(KeyStoreError::VaultLocked)
        ));
        assert!(matches!(store.load(URL), Err(KeyStoreError::VaultLocked)));
        assert!(matches!(store.remove(URL), Err(KeyStoreError::VaultLocked)));
    }

    #[test]
    fn unlocking_a_fresh_vault_creates_it_and_roundtrips_keys() {
        let dir = tempfile::tempdir().unwrap();
        let store = locked_store(&dir);
        store.unlock_file_vault("pass phrase").unwrap();
        assert_eq!(
            store.status(),
            StorageStatus {
                level: StorageLevel::EncryptedFile,
                locked: false,
                fallback_has_entries: false,
            }
        );
        store.store(URL, &ApiKey::new(KEY.into())).unwrap();
        assert_eq!(store.load(URL).unwrap().unwrap().expose(), KEY);
        store.remove(URL).unwrap();
        assert!(store.load(URL).unwrap().is_none());
    }

    #[test]
    fn unlocking_an_existing_vault_with_the_wrong_passphrase_fails_and_stays_locked() {
        let dir = tempfile::tempdir().unwrap();
        let store = locked_store(&dir);
        store.unlock_file_vault("right").unwrap();
        store.store(URL, &ApiKey::new(KEY.into())).unwrap();

        let reopened = locked_store(&dir);
        assert!(matches!(
            reopened.unlock_file_vault("wrong"),
            Err(KeyStoreError::Vault(VaultError::WrongPassphraseOrCorrupt))
        ));
        assert!(
            reopened.status().locked,
            "a failed unlock that flipped the status would let key writes race into \
             an unusable backend"
        );
        reopened.unlock_file_vault("right").unwrap();
        assert_eq!(reopened.load(URL).unwrap().unwrap().expose(), KEY);
    }

    #[test]
    fn session_only_keys_live_in_memory_and_leave_no_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let store = locked_store(&dir);
        store.use_session_only().unwrap();
        assert_eq!(
            store.status(),
            StorageStatus {
                level: StorageLevel::SessionOnly,
                locked: false,
                fallback_has_entries: false,
            }
        );
        store.store(URL, &ApiKey::new(KEY.into())).unwrap();
        assert_eq!(store.load(URL).unwrap().unwrap().expose(), KEY);
        assert!(
            !dir.path().join("keys.vault").exists(),
            "session-only means never persisted; a vault file appearing here would be \
             the forbidden plaintext-adjacent level"
        );
    }

    #[test]
    fn session_only_cannot_displace_an_available_keychain() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.vault");
        let store = KeyStore::with_forced_backend(path, Backend::Keyring);
        assert!(
            matches!(
                store.use_session_only(),
                Err(KeyStoreError::KeychainPresent)
            ),
            "the chain is ranked, not a free choice: dropping below the OS keychain while \
             it works would downgrade durability for no security gain"
        );
        assert!(store.unlock_file_vault("pw").is_ok());
        assert_eq!(
            store.status(),
            StorageStatus {
                level: StorageLevel::OsKeychain,
                locked: false,
                fallback_has_entries: false,
            }
        );
    }

    #[test]
    fn fallback_vault_loads_keys_when_keychain_is_active() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.vault");

        let file_store = KeyStore::with_forced_backend(
            path.clone(),
            Backend::FileUnlocked {
                vault: FileVault::new(path.clone()),
                passphrase: Zeroizing::new("secret-passphrase".to_string()),
            },
        );
        file_store.store(URL, &ApiKey::new(KEY.into())).unwrap();

        let keyring_store = KeyStore::with_forced_backend(path.clone(), Backend::Keyring);
        assert_eq!(
            keyring_store.status(),
            StorageStatus {
                level: StorageLevel::OsKeychain,
                locked: false,
                fallback_has_entries: true,
            }
        );

        keyring_store
            .unlock_file_vault("secret-passphrase")
            .unwrap();
        assert_eq!(
            keyring_store.status(),
            StorageStatus {
                level: StorageLevel::OsKeychain,
                locked: false,
                fallback_has_entries: true,
            }
        );

        let loaded = keyring_store.load(URL).unwrap();
        assert_eq!(
            loaded.map(|k| k.expose().to_string()),
            Some(KEY.to_string())
        );

        keyring_store.remove(URL).unwrap();
        assert!(keyring_store.load(URL).unwrap().is_none());

        let verify_store = KeyStore::with_forced_backend(
            path.clone(),
            Backend::FileUnlocked {
                vault: FileVault::new(path),
                passphrase: Zeroizing::new("secret-passphrase".to_string()),
            },
        );
        assert!(verify_store.load(URL).unwrap().is_none());
    }

    #[test]
    fn store_errors_render_without_key_material() {
        let dir = tempfile::tempdir().unwrap();
        let store = locked_store(&dir);
        let err = store.store(URL, &ApiKey::new(KEY.into())).unwrap_err();
        for rendered in [format!("{err}"), format!("{err:?}")] {
            assert!(
                !rendered.contains("fallback_secret"),
                "error strings are exactly what ends up in logs and UI toasts: {rendered}"
            );
        }
    }
}
