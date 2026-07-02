//! Admin account storage + password hashing.
//!
//! The CMS keeps its admin credentials in a sidecar `admin.toml` file —
//! deliberately *not* in the redb database, so a database wipe doesn't
//! lock the operator out and so the file can be rotated/backed up
//! independently. The format:
//!
//! ```toml
//! [admin]
//! username = "alice"
//! password_hash = "$argon2id$v=19$m=...$..."
//! created_at = "2026-04-26T10:00:00Z"
//!
//! cookie_key = "<base64 32 bytes>"
//! ```
//!
//! When the file doesn't exist on first boot, an empty store is created
//! (cookie key generated and saved). The setup wizard later writes the
//! `[admin]` section.

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use time::OffsetDateTime;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminAccount {
    pub username: String,
    /// Argon2id PHC string.
    pub password_hash: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// On-disk file shape. Internal to this module.
#[derive(Debug, Default, Serialize, Deserialize)]
struct AdminFile {
    #[serde(default)]
    admin: Option<AdminAccount>,
    /// Base64-encoded 32-byte cookie-signing key.
    cookie_key: String,
}

/// Runtime handle to the admin store. Cheap to clone (Arc of inner state).
pub struct AdminStore {
    path: PathBuf,
    inner: RwLock<Inner>,
    /// Raw cookie-signing bytes. Stable for the process lifetime.
    pub cookie_key: Vec<u8>,
}

struct Inner {
    admin: Option<AdminAccount>,
}

impl AdminStore {
    /// Load the sidecar at `path`. If absent, generate a cookie key and
    /// write a new file (no admin set yet — first-run setup will write it).
    pub fn load_or_init(path: &Path) -> Result<Self, AuthError> {
        let (admin, cookie_key_b64) = if path.exists() {
            let raw = std::fs::read_to_string(path)?;
            let parsed: AdminFile = toml::from_str(&raw).map_err(AuthError::Parse)?;
            (parsed.admin, parsed.cookie_key)
        } else {
            (None, generate_cookie_key_b64())
        };

        let cookie_key = decode_cookie_key(&cookie_key_b64)?;

        let store = Self {
            path: path.to_path_buf(),
            inner: RwLock::new(Inner { admin }),
            cookie_key,
        };
        // Persist if we just generated the key (file didn't exist).
        if !path.exists() {
            store.persist()?;
        }
        Ok(store)
    }

    /// True iff an admin account has been configured (setup complete).
    pub fn is_initialized(&self) -> bool {
        self.inner
            .read()
            .expect("admin store poisoned")
            .admin
            .is_some()
    }

    /// Snapshot of the current admin account.
    pub fn admin(&self) -> Option<AdminAccount> {
        self.inner
            .read()
            .expect("admin store poisoned")
            .admin
            .clone()
    }

    /// Create the admin account during first-run setup. Returns an error
    /// if an account already exists — setup is one-shot.
    pub fn create_admin(&self, username: &str, password: &str) -> Result<(), AuthError> {
        // Store the username trimmed: mobile keyboards and autofill love
        // appending a space, and an invisible trailing space in the
        // account file makes every future login "mysteriously" fail.
        // Passwords are NOT trimmed — whitespace is legal there.
        let username = username.trim();
        let mut guard = self.inner.write().expect("admin store poisoned");
        if guard.admin.is_some() {
            return Err(AuthError::AlreadyInitialized);
        }
        if username.is_empty() {
            return Err(AuthError::Validation("username must not be empty".into()));
        }
        if password.len() < 8 {
            return Err(AuthError::Validation(
                "password must be at least 8 characters".into(),
            ));
        }
        let hash = hash_password(password)?;
        guard.admin = Some(AdminAccount {
            username: username.into(),
            password_hash: hash,
            created_at: OffsetDateTime::now_utc(),
        });
        drop(guard);
        self.persist()?;
        Ok(())
    }

    /// Verify a login attempt. Constant-time-ish — argon2 verify dominates
    /// any branch on username comparison.
    pub fn verify_login(&self, username: &str, password: &str) -> bool {
        let admin = match self.admin() {
            Some(a) => a,
            None => return false,
        };
        // Trim both sides: tolerates sloppy login input AND accounts
        // created before create_admin trimmed on write.
        if admin.username.trim() != username.trim() {
            return false;
        }
        verify_password(password, &admin.password_hash).unwrap_or(false)
    }

    fn persist(&self) -> Result<(), AuthError> {
        let admin = self
            .inner
            .read()
            .expect("admin store poisoned")
            .admin
            .clone();
        let file = AdminFile {
            admin,
            cookie_key: encode_cookie_key(&self.cookie_key),
        };
        let raw = toml::to_string_pretty(&file).map_err(AuthError::Serialize)?;
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::write(&self.path, raw)?;
        Ok(())
    }
}

fn hash_password(password: &str) -> Result<String, AuthError> {
    let salt = SaltString::generate(&mut rand_core::OsRng);
    let argon2 = Argon2::default();
    let hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| AuthError::Hash(e.to_string()))?;
    Ok(hash.to_string())
}

fn verify_password(password: &str, phc: &str) -> Result<bool, AuthError> {
    let parsed = PasswordHash::new(phc).map_err(|e| AuthError::Hash(e.to_string()))?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

fn generate_cookie_key_b64() -> String {
    // tower_cookies::Key (signing + encryption) requires 64 bytes.
    let mut bytes = [0u8; 64];
    rand::thread_rng().fill_bytes(&mut bytes);
    encode_cookie_key(&bytes)
}

fn encode_cookie_key(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{:02x}", b);
    }
    s
}

fn decode_cookie_key(hex: &str) -> Result<Vec<u8>, AuthError> {
    if !hex.len().is_multiple_of(2) {
        return Err(AuthError::Validation(
            "cookie key must be hex-encoded (even length)".into(),
        ));
    }
    let mut out = Vec::with_capacity(hex.len() / 2);
    for chunk in hex.as_bytes().chunks(2) {
        let s = std::str::from_utf8(chunk)
            .map_err(|_| AuthError::Validation("cookie key not UTF-8".into()))?;
        let byte = u8::from_str_radix(s, 16)
            .map_err(|_| AuthError::Validation("cookie key not valid hex".into()))?;
        out.push(byte);
    }
    if out.len() < 64 {
        return Err(AuthError::Validation(
            "cookie key must decode to at least 64 bytes (signing + encryption)".into(),
        ));
    }
    Ok(out)
}

#[derive(Debug)]
pub enum AuthError {
    Io(std::io::Error),
    Parse(toml::de::Error),
    Serialize(toml::ser::Error),
    Hash(String),
    Validation(String),
    AlreadyInitialized,
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::Io(e) => write!(f, "I/O: {}", e),
            AuthError::Parse(e) => write!(f, "parse: {}", e),
            AuthError::Serialize(e) => write!(f, "serialize: {}", e),
            AuthError::Hash(s) => write!(f, "hash: {}", s),
            AuthError::Validation(s) => write!(f, "{}", s),
            AuthError::AlreadyInitialized => {
                write!(f, "admin account already exists; setup is one-shot")
            }
        }
    }
}

impl std::error::Error for AuthError {}

impl From<std::io::Error> for AuthError {
    fn from(e: std::io::Error) -> Self {
        AuthError::Io(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn tmp_path(name: &str) -> PathBuf {
        let mut p = env::temp_dir();
        p.push(format!("ccc-cms-auth-test-{name}-{}", std::process::id()));
        p.set_extension("toml");
        p
    }

    #[test]
    fn load_or_init_generates_and_persists_cookie_key() {
        let path = tmp_path("init");
        let _ = std::fs::remove_file(&path);
        let store = AdminStore::load_or_init(&path).unwrap();
        assert!(!store.is_initialized());
        // tower_cookies::Key needs 64 bytes (signing + encryption).
        assert!(store.cookie_key.len() >= 64);
        assert!(path.exists(), "sidecar should be written on first init");

        // Reload keeps the same key — sessions must survive restarts.
        let reloaded = AdminStore::load_or_init(&path).unwrap();
        assert_eq!(reloaded.cookie_key, store.cookie_key);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn create_admin_and_verify_login() {
        let path = tmp_path("verify");
        let _ = std::fs::remove_file(&path);
        let store = AdminStore::load_or_init(&path).unwrap();
        store.create_admin("alice", "correct horse").unwrap();
        assert!(store.is_initialized());

        assert!(store.verify_login("alice", "correct horse"));
        assert!(!store.verify_login("alice", "wrong password"));
        assert!(!store.verify_login("bob", "correct horse"));

        // Second setup attempt must fail — setup is one-shot.
        assert!(matches!(
            store.create_admin("mallory", "password123"),
            Err(AuthError::AlreadyInitialized)
        ));

        // Credentials survive a reload from disk.
        let reloaded = AdminStore::load_or_init(&path).unwrap();
        assert!(reloaded.verify_login("alice", "correct horse"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn usernames_are_whitespace_tolerant() {
        // The real-world failure: mobile autofill appends a space during
        // setup, then login with the clean username fails forever.
        let path = tmp_path("trim");
        let _ = std::fs::remove_file(&path);
        let store = AdminStore::load_or_init(&path).unwrap();
        store.create_admin("Lucerna ", "correct horse").unwrap();
        // Stored canonically…
        assert_eq!(store.admin().unwrap().username, "Lucerna");
        // …and login tolerates sloppy input on either side.
        assert!(store.verify_login("Lucerna", "correct horse"));
        assert!(store.verify_login("  Lucerna  ", "correct horse"));
        // Passwords are never trimmed.
        assert!(!store.verify_login("Lucerna", "correct horse "));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn create_admin_validates_input() {
        let path = tmp_path("validate");
        let _ = std::fs::remove_file(&path);
        let store = AdminStore::load_or_init(&path).unwrap();
        assert!(matches!(
            store.create_admin("", "password123"),
            Err(AuthError::Validation(_))
        ));
        assert!(matches!(
            store.create_admin("alice", "short"),
            Err(AuthError::Validation(_))
        ));
        assert!(!store.is_initialized());
        let _ = std::fs::remove_file(&path);
    }
}
