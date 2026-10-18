//! Key-encryption keys, which seal the secrets a log has to carry.
//!
//! Every replica of a mint must be able to lead, so each needs the secret of
//! every live generation key. A rotation makes its key on whichever replica
//! leads, and the log is the only thing the replicas share, so the rotation's
//! log entry carries the new key's secret sealed by a key-encryption key
//! (KEK) that every replica can use. Each replica unseals it as it applies
//! the entry. The log never holds a secret in the clear, and
//! [`FileLog::export`](crate::FileLog::export), what an auditor is given,
//! leaves out the sealed ones as well.
//!
//! [`Kek`] is where a cloud KMS fits in. [`FileKek`] keeps the key in a local
//! file, for development and tests.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use thiserror::Error;
use zeroize::Zeroizing;

use crate::keys::{GenerationKey, PublicKey, SECRET_KEY_LEN};

/// What a sealed generation key is bound to, ahead of the issuer key, the
/// generation and the key's public half.
const CONTEXT: &[u8] = b"worgl/sealed-generation-key/v1";
const SEALED_VERSION: u8 = 1;
const NONCE_LEN: usize = 24;

/// A KEK could not seal or unseal a secret.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum KekError {
    /// The sealed bytes do not open under this KEK and context: another KEK
    /// sealed them, or they were damaged or moved to another key's place.
    #[error("the sealed secret does not open under this key-encryption key")]
    Rejected,
    /// The KEK could not be used at all, such as a KMS that cannot be
    /// reached.
    #[error("the key-encryption key is unavailable: {0}")]
    Unavailable(String),
}

/// A key-encryption key: it seals 32-byte secrets that only it can unseal.
pub trait Kek: fmt::Debug + Send + Sync {
    /// Seals `secret`, bound to `context`: unsealing under any other context
    /// fails.
    fn seal(&self, secret: &[u8; SECRET_KEY_LEN], context: &[u8]) -> Result<Vec<u8>, KekError>;

    /// Unseals what [`seal`](Self::seal) returned for the same `context`.
    fn unseal(
        &self,
        sealed: &[u8],
        context: &[u8],
    ) -> Result<Zeroizing<[u8; SECRET_KEY_LEN]>, KekError>;
}

/// A KEK kept in a local file as 64 hex digits, readable by its owner only.
///
/// It seals with XChaCha20-Poly1305 under a fresh random nonce, with the
/// context as associated data. A sealed secret is a version byte (1), the
/// 24-byte nonce and the 48-byte ciphertext.
pub struct FileKek {
    cipher: XChaCha20Poly1305,
}

impl FileKek {
    /// Generates a KEK and writes it to a new file at `path`. Refuses to
    /// replace an existing file.
    pub fn create(path: &Path) -> io::Result<Self> {
        let mut key = Zeroizing::new([0u8; 32]);
        getrandom::fill(key.as_mut()).map_err(io::Error::other)?;
        let text = Zeroizing::new(hex::encode(*key) + "\n");
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(path)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        Ok(Self::from_bytes(&key))
    }

    /// Reads the KEK in the file at `path`, as [`create`](Self::create)
    /// wrote it.
    pub fn open(path: &Path) -> io::Result<Self> {
        let text = Zeroizing::new(fs::read_to_string(path)?);
        let mut key = Zeroizing::new([0u8; 32]);
        hex::decode_to_slice(text.trim(), key.as_mut()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} must hold 64 hex digits", path.display()),
            )
        })?;
        Ok(Self::from_bytes(&key))
    }

    /// A KEK from its 32 bytes.
    pub fn from_bytes(key: &[u8; 32]) -> Self {
        Self {
            cipher: XChaCha20Poly1305::new(&Key::from(*key)),
        }
    }
}

impl fmt::Debug for FileKek {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FileKek(..)")
    }
}

impl Kek for FileKek {
    fn seal(&self, secret: &[u8; SECRET_KEY_LEN], context: &[u8]) -> Result<Vec<u8>, KekError> {
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce).map_err(|error| {
            KekError::Unavailable(format!("no randomness for a nonce: {error}"))
        })?;
        let payload = Payload {
            msg: secret,
            aad: context,
        };
        let ciphertext = self
            .cipher
            .encrypt(&XNonce::from(nonce), payload)
            .map_err(|_| KekError::Unavailable("sealing failed".into()))?;
        let mut sealed = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
        sealed.push(SEALED_VERSION);
        sealed.extend_from_slice(&nonce);
        sealed.extend_from_slice(&ciphertext);
        Ok(sealed)
    }

    fn unseal(
        &self,
        sealed: &[u8],
        context: &[u8],
    ) -> Result<Zeroizing<[u8; SECRET_KEY_LEN]>, KekError> {
        let Some((&SEALED_VERSION, rest)) = sealed.split_first() else {
            return Err(KekError::Rejected);
        };
        if rest.len() < NONCE_LEN {
            return Err(KekError::Rejected);
        }
        let (nonce, ciphertext) = rest.split_at(NONCE_LEN);
        let nonce: [u8; NONCE_LEN] = nonce.try_into().expect("split at the nonce's length");
        let payload = Payload {
            msg: ciphertext,
            aad: context,
        };
        let plain = Zeroizing::new(
            self.cipher
                .decrypt(&XNonce::from(nonce), payload)
                .map_err(|_| KekError::Rejected)?,
        );
        let mut secret = Zeroizing::new([0u8; SECRET_KEY_LEN]);
        if plain.len() != SECRET_KEY_LEN {
            return Err(KekError::Rejected);
        }
        secret.copy_from_slice(&plain);
        Ok(secret)
    }
}

/// A generation key's secret sealed by a KEK, as a rotation's log entry
/// carries it to the replicas that did not make the key.
#[derive(Clone, PartialEq, Eq)]
pub struct SealedKey {
    /// The generation the key signs.
    pub generation: u64,
    /// The key's public half, which the rotation's certificate names.
    pub public: PublicKey,
    /// The secret, sealed under a context naming the issuer, the generation
    /// and the public half, so it opens in this one place only.
    pub sealed: Vec<u8>,
}

impl SealedKey {
    /// Seals `key`, the key of `issuer`'s generation `generation`.
    pub fn seal(
        kek: &dyn Kek,
        issuer: &PublicKey,
        generation: u64,
        key: &GenerationKey,
    ) -> Result<Self, KekError> {
        let public = key.public_key();
        let secret = Zeroizing::new(key.to_secret_bytes());
        let sealed = kek.seal(&secret, &context(issuer, generation, &public))?;
        Ok(Self {
            generation,
            public,
            sealed,
        })
    }

    /// Unseals the key of `issuer`'s generation, and checks it is the key
    /// named.
    pub fn unseal(&self, kek: &dyn Kek, issuer: &PublicKey) -> Result<GenerationKey, KekError> {
        let secret = kek.unseal(
            &self.sealed,
            &context(issuer, self.generation, &self.public),
        )?;
        let key = GenerationKey::from_secret_bytes(&secret);
        if key.public_key() != self.public {
            return Err(KekError::Rejected);
        }
        Ok(key)
    }
}

impl fmt::Debug for SealedKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SealedKey")
            .field("generation", &self.generation)
            .field("public", &self.public)
            .field("sealed", &format_args!("{} bytes", self.sealed.len()))
            .finish()
    }
}

fn context(issuer: &PublicKey, generation: u64, public: &PublicKey) -> Vec<u8> {
    [
        CONTEXT,
        issuer.as_bytes(),
        &generation.to_be_bytes(),
        public.as_bytes(),
    ]
    .concat()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::IssuerKey;

    #[test]
    fn a_sealed_key_opens_only_in_its_place_and_under_its_kek() {
        let kek = FileKek::from_bytes(&[7; 32]);
        let issuer = IssuerKey::generate().public_key();
        let key = GenerationKey::generate();
        let sealed = SealedKey::seal(&kek, &issuer, 5, &key).unwrap();
        assert_eq!(sealed.sealed.len(), 1 + 24 + 48);
        let secret = key.to_secret_bytes();
        assert!(!sealed.sealed.windows(32).any(|w| w == secret));
        let opened = sealed.unseal(&kek, &issuer).unwrap();
        assert_eq!(opened.to_secret_bytes(), secret);

        let other = FileKek::from_bytes(&[8; 32]);
        assert_eq!(
            sealed.unseal(&other, &issuer).unwrap_err(),
            KekError::Rejected
        );
        let foreign = IssuerKey::generate().public_key();
        assert_eq!(
            sealed.unseal(&kek, &foreign).unwrap_err(),
            KekError::Rejected
        );
        let moved = SealedKey {
            generation: 6,
            ..sealed.clone()
        };
        assert_eq!(moved.unseal(&kek, &issuer).unwrap_err(), KekError::Rejected);
        let mut damaged = sealed.clone();
        *damaged.sealed.last_mut().unwrap() ^= 1;
        assert_eq!(
            damaged.unseal(&kek, &issuer).unwrap_err(),
            KekError::Rejected
        );
        for short in [0, 1, 24, 40] {
            let cut = SealedKey {
                sealed: sealed.sealed[..short].to_vec(),
                ..sealed.clone()
            };
            assert_eq!(cut.unseal(&kek, &issuer).unwrap_err(), KekError::Rejected);
        }
        // Two seals of one key differ, by their nonces.
        let again = SealedKey::seal(&kek, &issuer, 5, &key).unwrap();
        assert_ne!(again.sealed, sealed.sealed);
        assert!(!format!("{sealed:?}").contains(&hex::encode(&sealed.sealed)));
    }

    #[test]
    fn a_file_kek_round_trips_and_is_private() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kek.key");
        let kek = FileKek::create(&path).unwrap();
        assert_eq!(
            FileKek::create(&path).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let issuer = IssuerKey::generate().public_key();
        let key = GenerationKey::generate();
        let sealed = SealedKey::seal(&kek, &issuer, 1, &key).unwrap();
        let reopened = FileKek::open(&path).unwrap();
        assert_eq!(
            sealed.unseal(&reopened, &issuer).unwrap().public_key(),
            key.public_key()
        );
        fs::write(&path, "not hex\n").unwrap();
        assert_eq!(
            FileKek::open(&path).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(format!("{kek:?}"), "FileKek(..)");
    }
}
