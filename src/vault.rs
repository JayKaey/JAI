//! The Personal Manager's credential vault.
//!
//! * Secrets (app tokens, app-specific passwords, WebUntis session ids) are sealed with
//!   AES-256-GCM in `vault_secrets` (`jarvis_memory.db`); each ciphertext is bound to its service
//!   and slot as associated data, so rows cannot be swapped.
//! * The 256-bit master key lives in the OS keyring (Windows Credential Manager). Where no keyring
//!   is available (headless Linux) it falls back to `jarvis_vault.key` next to the database.
//! * Non-sensitive settings (hosts, URLs, usernames, intervals) live in `service_configs`
//!   (`pim_config`), never here.

use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::Engine;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tracing::{info, warn};

const KEY_FILE: &str = "jarvis_vault.key";
#[cfg(windows)]
const KEYRING_SERVICE: &str = "J.A.R.V.I.S. Personal Manager";
#[cfg(windows)]
const KEYRING_USER: &str = "vault-master-key";

/// The secret slot of a service: its password / app token.
pub const TOKEN: &str = "token";
/// A manually supplied session id (WebUntis behind SSO).
pub const SESSION: &str = "session";

struct Inner {
    conn: Mutex<Connection>,
    cipher: Option<Aes256Gcm>,
    key_store: String,
}

#[derive(Clone)]
pub struct Vault {
    inner: Arc<Inner>,
}

impl Vault {
    pub fn open<P: AsRef<Path>>(db_path: P) -> Result<Self, String> {
        let conn = Connection::open(db_path.as_ref()).map_err(|e| e.to_string())?;
        let dir = db_path.as_ref().parent().map(Path::to_path_buf).filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| PathBuf::from("."));
        Self::with(conn, Some(&dir))
    }

    /// Secrets for this session only (the database could not be opened).
    pub fn in_memory() -> Self {
        let conn = Connection::open_in_memory().expect("in-memory SQLite");
        Self::with(conn, None).expect("in-memory vault")
    }

    fn with(conn: Connection, key_dir: Option<&Path>) -> Result<Self, String> {
        conn.busy_timeout(Duration::from_secs(3)).map_err(|e| e.to_string())?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS vault_secrets (
                 service TEXT NOT NULL,
                 name TEXT NOT NULL,
                 nonce BLOB NOT NULL,
                 ciphertext BLOB NOT NULL,
                 updated_at TEXT NOT NULL,
                 PRIMARY KEY (service, name)
             );",
        )
        .map_err(|e| e.to_string())?;
        let (cipher, key_store) = match key_dir {
            Some(dir) => match master_key(dir) {
                Ok((key, store)) => (Some(Aes256Gcm::new(&key)), store),
                Err(e) => {
                    warn!("[VAULT] No master key ({e}); secrets cannot be stored.");
                    (None, "unavailable".to_string())
                }
            },
            None => (Some(Aes256Gcm::new(&Aes256Gcm::generate_key(OsRng))), "memory (this session only)".to_string()),
        };
        info!("[VAULT] Master key in {key_store}");
        Ok(Self { inner: Arc::new(Inner { conn: Mutex::new(conn), cipher, key_store }) })
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.inner.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Where the master key is kept (shown in the Integrations hub).
    pub fn key_store(&self) -> &str {
        &self.inner.key_store
    }

    fn aad(service: &str, name: &str) -> Vec<u8> {
        format!("jarvis-vault|{service}|{name}").into_bytes()
    }

    pub fn set_secret(&self, service: &str, name: &str, value: &str) -> Result<(), String> {
        let cipher = self.inner.cipher.as_ref().ok_or("The vault has no master key, so secrets cannot be stored.")?;
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let aad = Self::aad(service, name);
        let ciphertext = cipher.encrypt(&nonce, Payload { msg: value.as_bytes(), aad: &aad }).map_err(|_| "encryption failed".to_string())?;
        self.conn()
            .execute(
                "INSERT INTO vault_secrets (service, name, nonce, ciphertext, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(service, name) DO UPDATE SET nonce = excluded.nonce, ciphertext = excluded.ciphertext, updated_at = excluded.updated_at",
                params![service, name, nonce.as_slice(), ciphertext, chrono::Local::now().to_rfc3339()],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn secret(&self, service: &str, name: &str) -> Option<String> {
        let cipher = self.inner.cipher.as_ref()?;
        let (nonce, ciphertext): (Vec<u8>, Vec<u8>) = self
            .conn()
            .query_row("SELECT nonce, ciphertext FROM vault_secrets WHERE service = ?1 AND name = ?2", params![service, name], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
            .ok()
            .flatten()?;
        if nonce.len() != 12 {
            return None;
        }
        let aad = Self::aad(service, name);
        match cipher.decrypt(Nonce::from_slice(&nonce), Payload { msg: &ciphertext, aad: &aad }) {
            Ok(plain) => String::from_utf8(plain).ok(),
            Err(_) => {
                warn!("[VAULT] The secret '{name}' of '{service}' could not be decrypted (the master key changed?).");
                None
            }
        }
    }

    pub fn has_secret(&self, service: &str, name: &str) -> bool {
        self.conn()
            .query_row("SELECT 1 FROM vault_secrets WHERE service = ?1 AND name = ?2", params![service, name], |_| Ok(()))
            .optional()
            .ok()
            .flatten()
            .is_some()
    }

    pub fn clear_secret(&self, service: &str, name: &str) {
        let _ = self.conn().execute("DELETE FROM vault_secrets WHERE service = ?1 AND name = ?2", params![service, name]);
    }

    /// The password / app token of a service.
    pub fn token(&self, service: &str) -> Option<String> {
        self.secret(service, TOKEN).filter(|t| !t.is_empty())
    }
}

// --------------------------------------------------------------------------- master key ----

fn decode_key(b64: &str) -> Result<Key<Aes256Gcm>, String> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64.trim()).map_err(|e| e.to_string())?;
    if bytes.len() != 32 {
        return Err("the stored master key has the wrong length".into());
    }
    Ok(*Key::<Aes256Gcm>::from_slice(&bytes))
}

fn encode_key(key: &Key<Aes256Gcm>) -> String {
    base64::engine::general_purpose::STANDARD.encode(key.as_slice())
}

/// The vault's master key and where it is kept. A key file from an earlier fallback keeps
/// winning, so secrets sealed with it stay readable.
fn master_key(dir: &Path) -> Result<(Key<Aes256Gcm>, String), String> {
    let file = dir.join(KEY_FILE);
    if file.is_file() {
        let key = decode_key(&std::fs::read_to_string(&file).map_err(|e| e.to_string())?)?;
        return Ok((key, format!("{} (local key file)", file.display())));
    }
    #[cfg(windows)]
    match keyring_key() {
        Ok(key) => return Ok((key, "Windows Credential Manager".to_string())),
        Err(e) => warn!("[VAULT] Windows Credential Manager unavailable ({e}); using a local key file."),
    }
    let key = Aes256Gcm::generate_key(OsRng);
    std::fs::write(&file, encode_key(&key)).map_err(|e| format!("cannot write {}: {e}", file.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600));
    }
    Ok((key, format!("{} (local key file)", file.display())))
}

#[cfg(windows)]
fn keyring_key() -> Result<Key<Aes256Gcm>, String> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER).map_err(|e| e.to_string())?;
    match entry.get_password() {
        Ok(stored) => decode_key(&stored),
        Err(keyring::Error::NoEntry) => {
            let key = Aes256Gcm::generate_key(OsRng);
            entry.set_password(&encode_key(&key)).map_err(|e| e.to_string())?;
            info!("[VAULT] Created a new master key in the Windows Credential Manager.");
            Ok(key)
        }
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_round_trip_and_are_bound_to_their_slot() {
        let v = Vault::in_memory();
        v.set_secret("webuntis", TOKEN, "geheim!").unwrap();
        assert_eq!(v.token("webuntis").as_deref(), Some("geheim!"));
        assert!(v.has_secret("webuntis", TOKEN));
        // A ciphertext copied into another slot does not decrypt.
        {
            let conn = v.conn();
            conn.execute(
                "INSERT INTO vault_secrets (service, name, nonce, ciphertext, updated_at) SELECT service, 'session', nonce, ciphertext, updated_at FROM vault_secrets WHERE name = 'token'",
                [],
            )
            .unwrap();
        }
        assert_eq!(v.secret("webuntis", SESSION), None);
        v.clear_secret("webuntis", TOKEN);
        assert_eq!(v.token("webuntis"), None);
    }
}
