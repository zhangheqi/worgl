//! A purse file encrypted under a key derived from a passphrase, in the
//! format of [`worgl_sdk::vault`]: the key derivation's parameters and salt
//! in the clear, then the holder, sealed with XChaCha20-Poly1305 under a
//! fresh random nonce on every save. The parameters are authenticated with
//! it, so they cannot be weakened without the passphrase.
//!
//! A purse file another program sealed, such as a backup of the TypeScript
//! SDK's, may name the server of each of its mints. `worgl` keeps that mint
//! list as it was each time it saves.
//!
//! Several `worgl` commands can run on one purse at once: each change locks
//! the file, decrypts what is saved now, changes that and saves it before
//! unlocking.

use std::fs;
use std::io;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use anyhow::{Context, Result, anyhow, bail};
use worgl_client::{FileStore, Locked, Store};
use worgl_sdk::vault::{self, MintEntry, VaultError};
use worgl_sdk::{Holder, LoadError};

/// A purse file, the key that opens it, and the mint list it holds.
pub(crate) struct Vault {
    file: FileStore,
    vault: vault::Vault,
    /// As the file held it when last read, to seal again with the holder.
    mints: Mutex<Vec<MintEntry>>,
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
        let vault = Self {
            file: FileStore::new(path),
            vault: vault::Vault::new(passphrase).map_err(|error| explain(path, error))?,
            mints: Mutex::default(),
        };
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
        let (vault, contents) =
            vault::Vault::open(&text, passphrase).map_err(|error| explain(path, error))?;
        let vault = Self {
            file: FileStore::new(path),
            vault,
            mints: Mutex::new(contents.mints),
        };
        Ok((vault, contents.holder))
    }

    fn mints(&self) -> MutexGuard<'_, Vec<MintEntry>> {
        // The list is only ever replaced whole, so a panic while another
        // thread held it left a whole list.
        self.mints.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Store for Vault {
    fn save(&self, json: &str) -> io::Result<()> {
        let holder = Holder::from_json(json)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let text = self
            .vault
            .seal(&holder, &self.mints())
            .map_err(io::Error::other)?;
        self.file.save(&text)
    }

    fn lock(&self) -> io::Result<Option<Locked>> {
        let lock = self.file.lock_file()?;
        let path = self.file.path();
        let text = fs::read_to_string(path)?;
        let contents = self.vault.unseal(&text).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{:#}", explain(path, error)),
            )
        })?;
        *self.mints() = contents.mints;
        Ok(Some(Locked::new(Some(contents.holder.to_json()), lock)))
    }
}

/// Says what is wrong with the purse file at `path`.
fn explain(path: &Path, error: VaultError) -> anyhow::Error {
    let path = path.display();
    match error {
        VaultError::NotSealed => anyhow!("{path} is not a purse file"),
        VaultError::Version(_) | VaultError::Holder(LoadError::Format(_)) => {
            anyhow!("{path} is a purse file of a format this version cannot read")
        }
        VaultError::Passphrase => anyhow!("wrong passphrase, or {path} is damaged"),
        VaultError::Holder(LoadError::Json(_)) | VaultError::Damaged(_) => {
            anyhow::Error::new(error).context(format!("{path} is damaged"))
        }
        VaultError::Memory | VaultError::Random => anyhow::Error::new(error),
    }
}
