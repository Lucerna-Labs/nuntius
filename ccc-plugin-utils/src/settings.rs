//! Generic on-disk settings store backed by a TOML file.
//!
//! Plugin settings are typically a single flat struct that needs to
//! survive process restarts. Before this crate, every plugin that
//! wanted persisted settings re-implemented the same 30 lines:
//! `RwLock<T>`, `load_or_init`, `snapshot`, `replace`, `persist`.
//!
//! [`TomlSidecar<T>`] collapses all of that to a few lines per plugin.
//! For more elaborate needs (e.g. ActivityPub's auto-generated RSA
//! keypair, which has to be merged into the loaded struct), keep a
//! hand-rolled store; the abstraction is not worth forcing.
//!
//! The TOML file's *entire* contents are the type `T`. There is no
//! `[section]` wrapper. If you have a single struct for your plugin's
//! settings, this is what you want.
//!
//! # Example
//!
//! ```ignore
//! use ccc_plugin_utils::TomlSidecar;
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Serialize, Deserialize, Default)]
//! struct MySettings { port: u16, name: String }
//!
//! let store = TomlSidecar::<MySettings>::load_or_init("my.toml").unwrap();
//! let snap = store.snapshot();
//! store.replace(MySettings { port: 9090, name: "x".into() }).unwrap();
//! ```

use serde::{de::DeserializeOwned, Serialize};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// Errors that can come out of a [`TomlSidecar`]. Wraps I/O and TOML
/// parse/serialize errors. Callers can `?` this directly into a
/// `PluginError::InitError(_)` if they like.
#[derive(Debug)]
pub enum SettingsError {
    /// File or directory couldn't be read/written.
    Io(std::io::Error),
    /// TOML couldn't be parsed.
    Parse(toml::de::Error),
    /// TOML couldn't be serialized.
    Serialize(String),
}

impl std::fmt::Display for SettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SettingsError::Io(e) => write!(f, "settings I/O: {e}"),
            SettingsError::Parse(e) => write!(f, "settings TOML parse: {e}"),
            SettingsError::Serialize(e) => write!(f, "settings TOML serialize: {e}"),
        }
    }
}

impl std::error::Error for SettingsError {}

impl From<std::io::Error> for SettingsError {
    fn from(e: std::io::Error) -> Self {
        SettingsError::Io(e)
    }
}

impl From<toml::de::Error> for SettingsError {
    fn from(e: toml::de::Error) -> Self {
        SettingsError::Parse(e)
    }
}

/// On-disk settings store. Cloning is cheap: both sides share the
/// underlying `Arc<RwLock<T>>`, so a write on one clone is visible to all.
pub struct TomlSidecar<T> {
    path: PathBuf,
    inner: Arc<RwLock<T>>,
    _marker: PhantomData<T>,
}

impl<T> Clone for TomlSidecar<T> {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
            inner: self.inner.clone(),
            _marker: PhantomData,
        }
    }
}

impl<T> TomlSidecar<T>
where
    T: Serialize + DeserializeOwned + Default + Clone,
{
    /// Load settings from `path`. If the file doesn't exist, the store
    /// is created with `T::default()` and that default is persisted
    /// before returning.
    pub fn load_or_init(path: impl AsRef<Path>) -> Result<Self, SettingsError> {
        let path = path.as_ref();
        let value = if path.exists() {
            let raw = std::fs::read_to_string(path)?;
            toml::from_str(&raw)?
        } else {
            T::default()
        };
        let store = Self {
            path: path.to_path_buf(),
            inner: Arc::new(RwLock::new(value)),
            _marker: PhantomData,
        };
        if !path.exists() {
            store.persist()?;
        }
        Ok(store)
    }

    /// Read the current settings. Cheap: clones the inner value.
    ///
    /// A poisoned lock (a thread panicked while holding the guard) is
    /// recovered via `into_inner()` rather than propagating the panic:
    /// settings are plain data protected by short critical sections, so
    /// the value is never left in a torn state worth crashing over.
    pub fn snapshot(&self) -> T {
        match self.inner.read() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Replace the current settings and persist the change to disk.
    /// Recovers from a poisoned lock the same way [`Self::snapshot`] does.
    pub fn replace(&self, new: T) -> Result<(), SettingsError> {
        {
            let mut guard = match self.inner.write() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            *guard = new;
        }
        self.persist()
    }

    /// Update the current settings in place via a closure and persist
    /// the result. Useful when only one field needs to change.
    /// Recovers from a poisoned lock the same way [`Self::snapshot`] does.
    pub fn update<F>(&self, f: F) -> Result<(), SettingsError>
    where
        F: FnOnce(&mut T),
    {
        {
            let mut guard = match self.inner.write() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            f(&mut guard);
        }
        self.persist()
    }

    fn persist(&self) -> Result<(), SettingsError> {
        let snap = self.snapshot();
        let raw =
            toml::to_string_pretty(&snap).map_err(|e| SettingsError::Serialize(e.to_string()))?;
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::write(&self.path, raw)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::env;

    #[derive(Debug, Default, PartialEq, Serialize, Deserialize, Clone)]
    struct Demo {
        port: u16,
        name: String,
    }

    fn tmp_path(name: &str) -> PathBuf {
        let mut p = env::temp_dir();
        p.push(format!(
            "ccc-plugin-utils-test-{name}-{}",
            std::process::id()
        ));
        p.set_extension("toml");
        p
    }

    #[test]
    fn load_or_init_creates_with_default() {
        let path = tmp_path("default");
        let _ = std::fs::remove_file(&path);
        let store = TomlSidecar::<Demo>::load_or_init(&path).unwrap();
        assert_eq!(store.snapshot(), Demo::default());
        assert!(path.exists(), "file should be created on first init");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn round_trip_via_replace() {
        let path = tmp_path("replace");
        let _ = std::fs::remove_file(&path);
        let store = TomlSidecar::<Demo>::load_or_init(&path).unwrap();
        store
            .replace(Demo {
                port: 9090,
                name: "hello".into(),
            })
            .unwrap();
        let reloaded = TomlSidecar::<Demo>::load_or_init(&path).unwrap();
        assert_eq!(reloaded.snapshot().port, 9090);
        assert_eq!(reloaded.snapshot().name, "hello");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn update_via_closure() {
        let path = tmp_path("update");
        let _ = std::fs::remove_file(&path);
        let store = TomlSidecar::<Demo>::load_or_init(&path).unwrap();
        store.update(|d| d.port = 1234).unwrap();
        assert_eq!(store.snapshot().port, 1234);
        let reloaded = TomlSidecar::<Demo>::load_or_init(&path).unwrap();
        assert_eq!(reloaded.snapshot().port, 1234);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn survives_a_poisoned_lock() {
        let path = tmp_path("poison");
        let _ = std::fs::remove_file(&path);
        let store = TomlSidecar::<Demo>::load_or_init(&path).unwrap();
        store.update(|d| d.port = 4321).unwrap();
        // Poison the lock: panic on another thread while holding the
        // write guard. (The panic output in the test log is expected.)
        let clone = store.clone();
        let _ = std::thread::spawn(move || {
            let _guard = clone.inner.write().unwrap();
            panic!("deliberately poisoning the settings lock");
        })
        .join();
        // Every accessor must keep working instead of panicking.
        assert_eq!(store.snapshot().port, 4321);
        store
            .replace(Demo {
                port: 1,
                name: "after-poison".into(),
            })
            .unwrap();
        store.update(|d| d.port = 2).unwrap();
        assert_eq!(store.snapshot().port, 2);
        assert_eq!(store.snapshot().name, "after-poison");
        let _ = std::fs::remove_file(&path);
    }
}
