//! The Personal Manager's credential vault.
//!
//! * Secrets (app tokens, app-specific passwords, WebUntis session ids) are sealed with
//!   AES-256-GCM in `vault_secrets` (`jai_memory.db`); each ciphertext is bound to its service
//!   and slot as associated data, so rows cannot be swapped.
//! * The 256-bit master key lives in the OS keyring (Windows Credential Manager). Where no keyring
//!   is available (headless Linux) it falls back to `jai_vault.key` next to the database.
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

const KEY_FILE: &str = "jai_vault.key";
#[cfg(windows)]
const KEYRING_SERVICE: &str = "J.A.I. Personal Manager";
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

    /// What every secret is sealed together with, in front of its slot. It is part of the
    /// seal, not a label: a secret sealed under one tag opens under no other. (2026-11-11: a
    /// rename changed this text and none of Sir's four secrets could be read until they were
    /// sealed again, `reseal_from`.)
    const TAG: &'static str = "jai-vault";

    fn aad(service: &str, name: &str) -> Vec<u8> {
        Self::aad_under(Self::TAG, service, name)
    }

    fn aad_under(tag: &str, service: &str, name: &str) -> Vec<u8> {
        format!("{tag}|{service}|{name}").into_bytes()
    }

    /// Seals again, under today's tag, every secret that was sealed under `tag` (the tag of an
    /// earlier build). How many were sealed again; a secret that opens under today's tag
    /// already, or under neither, stays as it is. Run through the live check
    /// (`JAI_VAULT_SEALED_AS`), so only test builds carry it.
    #[cfg(test)]
    pub fn reseal_from(&self, tag: &str) -> Result<usize, String> {
        let cipher = self.inner.cipher.as_ref().ok_or("The vault has no master key.")?;
        let rows: Vec<(String, String, Vec<u8>, Vec<u8>)> = {
            let conn = self.conn();
            let mut stmt = conn.prepare("SELECT service, name, nonce, ciphertext FROM vault_secrets").map_err(|e| e.to_string())?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).map_err(|e| e.to_string())?.flatten().collect();
            rows
        };
        let mut sealed = 0;
        for (service, name, nonce, ciphertext) in rows {
            if nonce.len() != 12 {
                continue;
            }
            let open = |aad: Vec<u8>| cipher.decrypt(Nonce::from_slice(&nonce), Payload { msg: &ciphertext, aad: &aad }).ok();
            if open(Self::aad(&service, &name)).is_some() {
                continue;
            }
            if let Some(plain) = open(Self::aad_under(tag, &service, &name)).and_then(|p| String::from_utf8(p).ok()) {
                self.set_secret(&service, &name, &plain)?;
                sealed += 1;
            }
        }
        Ok(sealed)
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

    /// The entry the master key is stored under is an address, not a label: a rename of
    /// 2026-11-11 changed it, the next start made a fresh key and could read no secret.
    #[cfg(windows)]
    #[test]
    fn the_master_keys_entry_keeps_its_name() {
        assert_eq!((KEYRING_SERVICE, KEYRING_USER), ("J.A.I. Personal Manager", "vault-master-key"));
    }

    /// 2026-11-11: the tag a secret is sealed with was renamed with everything else, and the
    /// vault could read none of its four secrets. A secret of an earlier tag is sealed again
    /// under today's; the tag itself is pinned.
    #[test]
    fn a_secret_sealed_under_an_earlier_tag_is_sealed_again() {
        assert_eq!(Vault::TAG, "jai-vault", "part of every seal: changing it locks every stored secret");
        let v = Vault::in_memory();
        v.set_secret("calendar", TOKEN, "today's").unwrap();
        // One sealed by an earlier build, under its tag.
        let cipher = v.inner.cipher.as_ref().unwrap();
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let aad = Vault::aad_under("earlier-vault", "webuntis", TOKEN);
        let sealed = cipher.encrypt(&nonce, Payload { msg: b"geheim!", aad: &aad }).unwrap();
        v.conn().execute("INSERT INTO vault_secrets (service, name, nonce, ciphertext, updated_at) VALUES ('webuntis', ?1, ?2, ?3, '')", params![TOKEN, nonce.as_slice(), sealed]).unwrap();
        assert_eq!(v.token("webuntis"), None, "it does not open under today's tag");
        assert_eq!(v.reseal_from("some-other-vault"), Ok(0), "nor is it touched for a tag it was not sealed under");
        assert_eq!(v.reseal_from("earlier-vault"), Ok(1));
        assert_eq!((v.token("webuntis").as_deref(), v.token("calendar").as_deref()), (Some("geheim!"), Some("today's")));
        assert_eq!(v.reseal_from("earlier-vault"), Ok(0), "once");
    }

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
