//! A purse file encrypted under a key derived from a passphrase.
//!
//! The file is JSON: the key derivation's parameters and salt in the clear,
//! then the holder, sealed with XChaCha20-Poly1305 under a fresh random
//! nonce on every save. The parameters are authenticated with it, so they
//! cannot be weakened without the passphrase.
//!
//! Several `worgl` commands can run on one purse at once: each change locks
//! the file, decrypts what is saved now, changes that and saves it before
//! unlocking.

use std::fs;
use std::io;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use worgl_client::{FileStore, Locked, Store};
use worgl_sdk::Holder;
use zeroize::Zeroizing;

const FORMAT: &str = "worgl-purse";
const VERSION: u32 = 1;
const ALGORITHM: &str = "argon2id";

/// Argon2id with 64 MiB, three passes and one lane: the second of RFC
/// 9106's recommended settings.
const MEMORY_KIB: u32 = 64 * 1024;
const ITERATIONS: u32 = 3;
const PARALLELISM: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Kdf {
    algorithm: String,
    memory_kib: u32,
    iterations: u32,
    parallelism: u32,
    salt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Header {
    format: String,
    version: u32,
    kdf: Kdf,
}

#[derive(Serialize, Deserialize)]
struct Sealed {
    #[serde(flatten)]
    header: Header,
    nonce: String,
    ciphertext: String,
}

/// A purse file and the key that opens it.
pub(crate) struct Vault {
    file: FileStore,
    header: Header,
    aad: Vec<u8>,
    cipher: XChaCha20Poly1305,
}

impl Vault {
    /// Creates a purse file holding an empty holder. Fails if the file
    /// exists.
    pub(crate) fn create(path: &Path, passphrase: &str) -> Result<Self> {
        let _lock = FileStore::new(path)
            .lock_file()
            .with_context(|| format!("locking {}", path.display()))?;
        if path.exists() {
            bail!("{} already exists", path.display());
        }
        let mut salt = [0u8; 16];
        getrandom::fill(&mut salt).map_err(|e| anyhow!("the system RNG failed: {e}"))?;
        let header = Header {
            format: FORMAT.into(),
            version: VERSION,
            kdf: Kdf {
                algorithm: ALGORITHM.into(),
                memory_kib: MEMORY_KIB,
                iterations: ITERATIONS,
                parallelism: PARALLELISM,
                salt: hex::encode(salt),
            },
        };
        let vault = Self::unlock(path, header, passphrase)?;
        vault
            .save(&Holder::new().to_json())
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(vault)
    }

    /// Opens a purse file and decrypts the holder in it.
    pub(crate) fn open(path: &Path, passphrase: &str) -> Result<(Self, Holder)> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                bail!(
                    "no purse at {}; create one with `worgl init`",
                    path.display()
                )
            }
            Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
        };
        let sealed = parse(path, &text)?;
        let vault = Self::unlock(path, sealed.header.clone(), passphrase)?;
        let json = vault.unseal(&sealed)?;
        let holder = Holder::from_json(&json).context("the purse's contents are damaged")?;
        Ok((vault, holder))
    }

    /// Decrypts the holder's JSON. The header must be the one this vault
    /// was unlocked with, or the decryption fails.
    fn unseal(&self, sealed: &Sealed) -> Result<Zeroizing<String>> {
        let path = self.file.path();
        let mut nonce = [0u8; 24];
        hex::decode_to_slice(&sealed.nonce, &mut nonce).context("the purse's nonce is damaged")?;
        let ciphertext = STANDARD
            .decode(&sealed.ciphertext)
            .context("the purse's contents are damaged")?;
        let payload = Payload {
            msg: &ciphertext,
            aad: &self.aad,
        };
        let plain = Zeroizing::new(
            self.cipher
                .decrypt(&XNonce::from(nonce), payload)
                .map_err(|_| anyhow!("wrong passphrase, or {} is damaged", path.display()))?,
        );
        let json = std::str::from_utf8(&plain).context("the purse's contents are damaged")?;
        Ok(Zeroizing::new(json.to_owned()))
    }

    /// Derives the key for `header` from the passphrase.
    fn unlock(path: &Path, header: Header, passphrase: &str) -> Result<Self> {
        if header.kdf.algorithm != ALGORITHM {
            bail!("unknown key derivation {:?}", header.kdf.algorithm);
        }
        let salt = hex::decode(&header.kdf.salt).context("the purse's salt is damaged")?;
        let kdf = &header.kdf;
        let params = Params::new(kdf.memory_kib, kdf.iterations, kdf.parallelism, Some(32))
            .map_err(|e| anyhow!("the purse's key derivation settings are invalid: {e}"))?;
        let mut key = Zeroizing::new([0u8; 32]);
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password_into(passphrase.as_bytes(), &salt, key.as_mut())
            .map_err(|e| anyhow!("deriving the purse's key failed: {e}"))?;
        let cipher = XChaCha20Poly1305::new(&Key::from(*key));
        let aad = serde_json::to_vec(&header).expect("a header always serializes");
        Ok(Self {
            file: FileStore::new(path),
            header,
            aad,
            cipher,
        })
    }
}

impl Store for Vault {
    fn save(&self, json: &str) -> io::Result<()> {
        let mut nonce = [0u8; 24];
        getrandom::fill(&mut nonce).map_err(io::Error::other)?;
        let payload = Payload {
            msg: json.as_bytes(),
            aad: &self.aad,
        };
        let ciphertext = self
            .cipher
            .encrypt(&XNonce::from(nonce), payload)
            .map_err(|_| io::Error::other("encrypting the purse failed"))?;
        let sealed = Sealed {
            header: self.header.clone(),
            nonce: hex::encode(nonce),
            ciphertext: STANDARD.encode(ciphertext),
        };
        let text = serde_json::to_string_pretty(&sealed).map_err(io::Error::other)?;
        self.file.save(&text)
    }

    fn lock(&self) -> io::Result<Option<Locked>> {
        let lock = self.file.lock_file()?;
        let path = self.file.path();
        let text = fs::read_to_string(path)?;
        let json = parse(path, &text)
            .and_then(|sealed| self.unseal(&sealed))
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, format!("{error:#}")))?;
        Ok(Some(Locked::new(Some(json.to_string()), lock)))
    }
}

/// Reads a sealed purse file's JSON.
fn parse(path: &Path, text: &str) -> Result<Sealed> {
    let sealed: Sealed = serde_json::from_str(text)
        .with_context(|| format!("{} is not a purse file", path.display()))?;
    if sealed.header.format != FORMAT || sealed.header.version != VERSION {
        bail!(
            "{} is a purse file of a format this version cannot read",
            path.display()
        );
    }
    Ok(sealed)
}
