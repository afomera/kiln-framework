//! Typed configuration and encrypted secrets.
//!
//! ```ignore
//! // src/config.rs
//! #[derive(Config)]
//! pub struct Config {
//!     #[config(dev_default = "smtp://localhost:1025")]
//!     pub smtp_url: Secret<String>,          // SMTP_URL
//!     #[config(default = "hello@example.com")]
//!     pub mail_from: String,                  // MAIL_FROM
//!     pub sentry_dsn: Option<String>,         // optional
//! }
//! ```
//!
//! Each field comes from, in order: the environment variable named after it (`smtp_url` →
//! `SMTP_URL`), the encrypted secrets file (`kiln secrets set SMTP_URL ..`), `default`, or
//! (outside production) `dev_default`. Everything missing or invalid is reported at once,
//! at boot.

use std::{
    collections::BTreeMap,
    fmt,
    path::{Path, PathBuf},
    str::FromStr,
    sync::OnceLock,
};

use base64::{Engine, engine::general_purpose::STANDARD};
use chacha20poly1305::{
    ChaCha20Poly1305, KeyInit, Nonce,
    aead::{Aead, AeadCore, OsRng},
};

use crate::{Error, Result};

/// Implemented by `#[derive(Config)]`.
pub trait Config: Sized + Send + Sync + 'static {
    fn load() -> Result<Self>;
}

/// `development`, `test` or `production`: `KILN_ENV`, else `production` for release
/// builds and `development` otherwise. Test apps run as `test`.
static ENV: OnceLock<String> = OnceLock::new();

/// Test apps run as `test` (unless `KILN_ENV` says otherwise).
pub(crate) fn use_test_env() {
    let _ = ENV.set(std::env::var("KILN_ENV").unwrap_or_else(|_| "test".to_owned()));
}

pub fn env() -> &'static str {
    ENV.get_or_init(|| {
        std::env::var("KILN_ENV").unwrap_or_else(|_| {
            if cfg!(debug_assertions) { "development".to_owned() } else { "production".to_owned() }
        })
    })
}

pub fn is_production() -> bool {
    env() == "production"
}

/// A value that never prints: `Debug` and `Display` show `[redacted]`.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret<T>(T);

impl<T> Secret<T> {
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// The value itself. Searchable on purpose: every use of a secret is visible.
    pub fn expose(&self) -> &T {
        &self.0
    }
}

impl<T> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

impl<T> fmt::Display for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

impl<T: FromStr> FromStr for Secret<T> {
    type Err = T::Err;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        T::from_str(s).map(Secret)
    }
}

/// The app's root, set by `kiln::Cli` and the test harness, so `config/secrets.enc` can be
/// found.
static ROOT: OnceLock<PathBuf> = OnceLock::new();

pub(crate) fn set_root(root: &Path) {
    let _ = ROOT.set(root.to_owned());
}

/// Raw values for config loading: secrets file entries (when it can be decrypted).
fn secret_values() -> &'static BTreeMap<String, String> {
    static VALUES: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    VALUES.get_or_init(|| {
        let Some(root) = ROOT.get() else { return BTreeMap::new() };
        match Secrets::open(root) {
            Ok(Some(secrets)) => secrets.values,
            Ok(None) => BTreeMap::new(),
            Err(e) => {
                tracing::warn!(target: "kiln::config", error = %e, "couldn't read config/secrets.enc");
                BTreeMap::new()
            }
        }
    })
}

/// Field-loading helpers for `#[derive(Config)]`.
#[doc(hidden)]
pub struct Loader {
    problems: Vec<String>,
}

impl Loader {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self { problems: vec![] }
    }

    fn raw(&self, name: &str, default: Option<&str>, dev_default: Option<&str>) -> Option<String> {
        if let Ok(value) = std::env::var(name) {
            return Some(value);
        }
        if let Some(value) = secret_values().get(name) {
            return Some(value.clone());
        }
        if let Some(value) = default {
            return Some(value.to_owned());
        }
        dev_default.filter(|_| !is_production()).map(str::to_owned)
    }

    /// A required value. Records a problem and returns `None` when missing or invalid.
    pub fn required<T: FromStr>(&mut self, name: &str, default: Option<&str>, dev_default: Option<&str>) -> Option<T>
    where
        T::Err: fmt::Display,
    {
        match self.raw(name, default, dev_default) {
            Some(raw) => self.parse(name, &raw),
            None => {
                self.problems.push(format!(
                    "{name}: missing. Set the environment variable, or store it encrypted: `kiln secrets set {name} <value>`"
                ));
                None
            }
        }
    }

    /// An `Option<T>` field: missing is fine.
    pub fn optional<T: FromStr>(&mut self, name: &str, default: Option<&str>, dev_default: Option<&str>) -> Option<T>
    where
        T::Err: fmt::Display,
    {
        let raw = self.raw(name, default, dev_default)?;
        self.parse(name, &raw)
    }

    fn parse<T: FromStr>(&mut self, name: &str, raw: &str) -> Option<T>
    where
        T::Err: fmt::Display,
    {
        match raw.parse() {
            Ok(value) => Some(value),
            Err(e) => {
                self.problems.push(format!("{name}: `{raw}` is invalid ({e})"));
                None
            }
        }
    }

    pub fn finish(self) -> Result<()> {
        if self.problems.is_empty() {
            Ok(())
        } else {
            Err(Error::Config(format!("the app's config is incomplete ({}):\n  {}", env(), self.problems.join("\n  "))))
        }
    }
}

/// `config/secrets.enc`: `KEY=value` lines, encrypted with ChaCha20-Poly1305 under the key
/// in `KILN_MASTER_KEY` or `config/master.key`.
pub struct Secrets {
    path: PathBuf,
    key: [u8; 32],
    pub values: BTreeMap<String, String>,
}

impl Secrets {
    fn paths(root: &Path) -> (PathBuf, PathBuf) {
        (root.join("config/secrets.enc"), root.join("config/master.key"))
    }

    fn read_key(root: &Path) -> Result<Option<[u8; 32]>> {
        let hex = match std::env::var("KILN_MASTER_KEY") {
            Ok(key) => key,
            Err(_) => match std::fs::read_to_string(Self::paths(root).1) {
                Ok(key) => key,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e.into()),
            },
        };
        let hex = hex.trim();
        let bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .filter_map(|i| hex.get(i..i + 2).and_then(|b| u8::from_str_radix(b, 16).ok()))
            .collect();
        let key: [u8; 32] = bytes
            .try_into()
            .map_err(|_| Error::Config("the master key must be 64 hex characters (32 bytes)".into()))?;
        Ok(Some(key))
    }

    /// The decrypted secrets, or `None` when there's no master key or no secrets file.
    pub fn open(root: &Path) -> Result<Option<Self>> {
        let (path, _) = Self::paths(root);
        let Some(key) = Self::read_key(root)? else { return Ok(None) };
        let encoded = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Some(Self { path, key, values: BTreeMap::new() }));
            }
            Err(e) => return Err(e.into()),
        };
        let bytes = STANDARD
            .decode(encoded.trim())
            .map_err(|e| Error::Config(format!("config/secrets.enc isn't valid: {e}")))?;
        if bytes.len() < 12 {
            return Err(Error::Config("config/secrets.enc is truncated".into()));
        }
        let (nonce, ciphertext) = bytes.split_at(12);
        let plain = ChaCha20Poly1305::new(&key.into())
            .decrypt(Nonce::from_slice(nonce), ciphertext)
            .map_err(|_| Error::Config("can't decrypt config/secrets.enc: wrong master key?".into()))?;
        let text = String::from_utf8(plain).map_err(|_| Error::Config("config/secrets.enc isn't UTF-8".into()))?;
        Ok(Some(Self { path, key, values: parse_lines(&text) }))
    }

    /// Creates `config/master.key` (if there's no key yet) and an empty secrets file.
    pub fn init(root: &Path) -> Result<Self> {
        if let Some(secrets) = Self::open(root)? {
            return Ok(secrets);
        }
        let (path, key_path) = Self::paths(root);
        std::fs::create_dir_all(root.join("config"))?;
        let mut key = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut key);
        let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
        std::fs::write(&key_path, format!("{hex}\n"))?;
        let secrets = Self { path, key, values: BTreeMap::new() };
        secrets.save()?;
        Ok(secrets)
    }

    pub fn save(&self) -> Result<()> {
        let text: String = self.values.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
        let cipher = ChaCha20Poly1305::new(&self.key.into());
        let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
        let ciphertext = cipher
            .encrypt(&nonce, text.as_bytes())
            .map_err(|_| Error::Config("encrypting secrets failed".into()))?;
        let mut bytes = nonce.to_vec();
        bytes.extend(ciphertext);
        std::fs::write(&self.path, format!("{}\n", STANDARD.encode(bytes)))?;
        Ok(())
    }

    pub fn to_text(&self) -> String {
        self.values.iter().map(|(k, v)| format!("{k}={v}\n")).collect()
    }

    pub fn replace_from_text(&mut self, text: &str) {
        self.values = parse_lines(text);
    }
}

fn parse_lines(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('=').map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_round_trip_and_reject_wrong_keys() {
        let dir = std::env::temp_dir().join(format!("kiln-secrets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut secrets = Secrets::init(&dir).unwrap();
        secrets.values.insert("SMTP_URL".into(), "smtp://user:pw@host".into());
        secrets.save().unwrap();
        let reopened = Secrets::open(&dir).unwrap().unwrap();
        assert_eq!(reopened.values["SMTP_URL"], "smtp://user:pw@host");
        assert!(!std::fs::read_to_string(dir.join("config/secrets.enc")).unwrap().contains("smtp://"));

        std::fs::write(dir.join("config/master.key"), "00".repeat(32)).unwrap();
        assert!(Secrets::open(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn secret_values_never_print() {
        let secret: Secret<String> = "hunter2".parse().unwrap();
        assert_eq!(format!("{secret} {secret:?}"), "[redacted] [redacted]");
        assert_eq!(secret.expose(), "hunter2");
    }
}
