//! Purse files: a [`Holder`] sealed under a passphrase, in the format that
//! `worgl`, the command-line purse, keeps its purse in. What one program
//! seals, another opens: the TypeScript SDK seals backups that `worgl
//! --purse` opens, and opens the files `worgl` keeps. Needs the `vault`
//! feature.
//!
//! ```
//! use worgl_sdk::Holder;
//! use worgl_sdk::vault::Vault;
//!
//! let vault = Vault::new("correct horse battery staple")?;
//! let sealed = vault.seal(&Holder::new(), &[])?;
//!
//! let (_, opened) = Vault::open(&sealed, "correct horse battery staple")?;
//! assert_eq!(opened.holder.notes().count(), 0);
//! assert!(Vault::open(&sealed, "wrong").is_err());
//! # Ok::<(), worgl_sdk::vault::VaultError>(())
//! ```
//!
//! # The format
//!
//! A purse file is a JSON object, which `worgl` writes indented:
//!
//! ```json
//! {
//!   "format": "worgl-purse",
//!   "version": 1,
//!   "kdf": {
//!     "algorithm": "argon2id",
//!     "memory_kib": 65536,
//!     "iterations": 3,
//!     "parallelism": 1,
//!     "salt": "16 random bytes, in hex"
//!   },
//!   "nonce": "24 random bytes, in hex",
//!   "ciphertext": "base64, with padding"
//! }
//! ```
//!
//! The key is 32 bytes of Argon2id, version 0x13, over the passphrase's
//! UTF-8 bytes and the salt, with the settings `kdf` names: [`Vault::new`]
//! takes 64 MiB, three passes and one lane, the second of RFC 9106's
//! recommended settings. The ciphertext is XChaCha20-Poly1305 under that key
//! and the nonce, fresh for every seal, with the header as associated data:
//! `format`, `version` and `kdf`, as compact JSON in that order. So nobody
//! without the passphrase can weaken the settings or change the salt: the
//! file would no longer open.
//!
//! What it seals is the holder's JSON, as [`Holder::to_json`] writes it,
//! with one more member when the purse names its mints' servers: `mints`, a
//! list of [`MintEntry`]. A reader of a holder skips members it does not know, so
//! `worgl` 0.1 opens a file with a mint list too, and leaves the list out
//! the next time it saves. A file without one, such as any `worgl` 0.1
//! wrote, opens with an empty list.
//!
//! A version-1 file always derives its key with Argon2id; a writer that
//! changes that, or anything else a version-1 reader would misread, writes
//! another version, which this SDK refuses as [`VaultError::Version`].
//! Opening a file costs what its settings say, so a reader refuses settings
//! above [`MAX_MEMORY_KIB`], [`MAX_ITERATIONS`] or [`MAX_PARALLELISM`] as
//! damaged rather than spend minutes on them.

use argon2::{Algorithm, Argon2, Block, Params, Version};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Map, Value};
use worgl::PublicKey;
use zeroize::Zeroizing;

use crate::holder::{Holder, LoadError};

const FORMAT: &str = "worgl-purse";
const VERSION: u32 = 1;
const ALGORITHM: &str = "argon2id";

/// Argon2id with 64 MiB, three passes and one lane: the second of RFC
/// 9106's recommended settings.
const MEMORY_KIB: u32 = 64 * 1024;
const ITERATIONS: u32 = 3;
const PARALLELISM: u32 = 1;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;

/// Most memory, in KiB, a purse file's settings may ask of a reader: four
/// times what [`Vault::new`] takes, 256 MiB.
pub const MAX_MEMORY_KIB: u32 = 4 * MEMORY_KIB;

/// Most passes a purse file's settings may ask of a reader.
pub const MAX_ITERATIONS: u32 = 8;

/// Most lanes a purse file's settings may ask of a reader. This crate runs
/// them one after another, so more lanes cost no more time.
pub const MAX_PARALLELISM: u32 = 16;

/// A purse file cannot be sealed or opened.
#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    /// The text is not a purse file: not JSON with a purse file's `format`
    /// and `version`, or of another format, such as a holder saved
    /// unsealed.
    #[error("not a purse file")]
    NotSealed,
    /// It is a purse file of a version this SDK does not read: a newer one.
    #[error("a purse file of version {0}, which this SDK does not read")]
    Version(u32),
    /// A part of it does not decode, as named: its header, salt, nonce or
    /// ciphertext, its key derivation's settings, which may also be invalid
    /// or above what a reader spends, or the mint list sealed in it.
    #[error("the purse file's {0} is damaged")]
    Damaged(&'static str),
    /// It opened, but what it holds is no holder: saved by a newer SDK, or
    /// damaged before it was sealed.
    #[error("the purse file opened, but holds no holder this SDK reads: {0}")]
    Holder(LoadError),
    /// The passphrase does not open it: it is wrong, or the file was altered
    /// since it was sealed, its header included. [`Vault::unseal`] says so
    /// too for a file sealed under another salt or settings. So does a
    /// passphrase longer than Argon2id takes, 4 GiB, under which nothing is
    /// sealed.
    #[error("wrong passphrase, or the purse file was altered")]
    Passphrase,
    /// There is not enough memory to derive the key: 64 MiB for a file
    /// [`Vault::new`] made, or what the file's settings ask.
    #[error("not enough memory to derive the purse file's key")]
    Memory,
    /// The system's random number generator failed, so no salt or nonce
    /// could be drawn.
    #[error("the system random number generator failed")]
    Random,
    /// [`Vault::new`] was given an empty passphrase, which would let anyone
    /// who gets the file open it and spend its notes. A file is opened with
    /// whatever passphrase sealed it.
    #[error("the passphrase is empty")]
    EmptyPassphrase,
}

/// What a purse file holds: the holder, and where the purse reaches its
/// mints, if it said.
#[derive(Debug, Clone, Default)]
pub struct Contents {
    /// The holder: the notes, keys and transfers of every mint.
    pub holder: Holder,
    /// The servers of its mints, as the purse that sealed it named them.
    /// Empty for a file `worgl` 0.1 wrote.
    pub mints: Vec<MintEntry>,
}

impl Contents {
    /// The server the mint list names for `mint`: the first entry's, if
    /// there are several.
    pub fn server(&self, mint: &PublicKey) -> Option<&str> {
        self.mints
            .iter()
            .find(|entry| entry.mint == *mint)
            .map(|entry| entry.server.as_str())
    }
}

/// One entry of a purse file's mint list: a mint, and the server the purse
/// reaches it at.
///
/// A server's URL tells a purse where to ask, not who answers: the notes
/// check out only against keys that the mint's issuer key certifies,
/// whoever serves them.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct MintEntry {
    /// The mint's issuer key.
    pub mint: PublicKey,
    /// The URL of the server the purse reaches it at, such as
    /// `https://mint.example.com`.
    pub server: String,
    /// The entry's other members, such as a name a person gave the mint,
    /// kept as they were by a reader that does not know them. Members named
    /// `mint` or `server` here are never written.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl MintEntry {
    /// An entry naming `server` for `mint`, with nothing else.
    pub fn new(mint: PublicKey, server: impl Into<String>) -> Self {
        Self {
            mint,
            server: server.into(),
            extra: Map::new(),
        }
    }
}

impl Serialize for MintEntry {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let extra = self
            .extra
            .iter()
            .filter(|(name, _)| !matches!(name.as_str(), "mint" | "server"));
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("mint", &self.mint)?;
        map.serialize_entry("server", &self.server)?;
        for (name, value) in extra {
            map.serialize_entry(name, value)?;
        }
        map.end()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Kdf {
    algorithm: String,
    memory_kib: u32,
    iterations: u32,
    parallelism: u32,
    salt: String,
}

/// What the ciphertext is bound to, in the clear.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// The members a reader looks at first, to tell a purse file of another
/// version from a damaged one.
#[derive(Deserialize)]
struct Probe {
    format: String,
    version: u32,
}

/// The sealed holder's JSON and the mint list, as one object.
#[derive(Serialize)]
struct Plain<'a> {
    #[serde(flatten)]
    holder: crate::holder::Saved,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    mints: &'a [MintEntry],
}

/// The mint list of a sealed holder's JSON; the holder's own members are
/// skipped.
#[derive(Deserialize)]
struct Listed {
    #[serde(default)]
    mints: Vec<MintEntry>,
}

/// A key derived from a passphrase, and the header it was derived under:
/// what seals a purse file, and opens it again.
///
/// Deriving the key is the slow part, on purpose: Argon2id over 64 MiB,
/// three times. Keep the vault to seal the same file again after a change,
/// as `worgl` does: each seal then takes only a fresh nonce.
pub struct Vault {
    header: Header,
    aad: Vec<u8>,
    cipher: XChaCha20Poly1305,
}

impl std::fmt::Debug for Vault {
    // The cipher holds the key.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vault")
            .field("header", &self.header)
            .finish_non_exhaustive()
    }
}

impl Vault {
    /// Derives a key from `passphrase` under a fresh random salt, with
    /// Argon2id over 64 MiB, three passes and one lane, to seal a new purse
    /// file. An empty passphrase is refused, as
    /// [`VaultError::EmptyPassphrase`].
    pub fn new(passphrase: &str) -> Result<Self, VaultError> {
        if passphrase.is_empty() {
            return Err(VaultError::EmptyPassphrase);
        }
        let mut salt = [0u8; SALT_LEN];
        getrandom::fill(&mut salt).map_err(|_| VaultError::Random)?;
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
        Self::derive(header, passphrase)
    }

    /// Opens a purse file with `passphrase`: derives the key its header
    /// names and reads what it holds. Returns the vault too, which seals the
    /// file again under the same salt and settings.
    pub fn open(sealed: &str, passphrase: &str) -> Result<(Self, Contents), VaultError> {
        let (sealed, nonce, ciphertext) = parse(sealed)?;
        let vault = Self::derive(sealed.header, passphrase)?;
        let contents = vault.decrypt(&nonce, &ciphertext)?;
        Ok((vault, contents))
    }

    /// Opens a purse file this vault's key seals, such as one it sealed
    /// before: with no key to derive, it is quick. A file sealed under
    /// another salt or settings, such as one sealed again with
    /// [`new`](Self::new), does not open: [`VaultError::Passphrase`].
    pub fn unseal(&self, sealed: &str) -> Result<Contents, VaultError> {
        let (sealed, nonce, ciphertext) = parse(sealed)?;
        if sealed.header != self.header {
            return Err(VaultError::Passphrase);
        }
        self.decrypt(&nonce, &ciphertext)
    }

    /// Seals `holder` with the mint list `mints`, under a fresh nonce, as the
    /// indented JSON of a purse file. An empty list leaves `mints` out, as
    /// `worgl` 0.1 wrote.
    pub fn seal(&self, holder: &Holder, mints: &[MintEntry]) -> Result<String, VaultError> {
        let plain = Plain {
            holder: holder.saved(),
            mints,
        };
        let json = Zeroizing::new(serde_json::to_vec(&plain).expect("a holder always serializes"));
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce).map_err(|_| VaultError::Random)?;
        let payload = Payload {
            msg: &json,
            aad: &self.aad,
        };
        let ciphertext = self
            .cipher
            .encrypt(&XNonce::from(nonce), payload)
            .expect("XChaCha20-Poly1305 seals any holder: it takes up to 256 GiB");
        let sealed = Sealed {
            header: self.header.clone(),
            nonce: hex::encode(nonce),
            ciphertext: STANDARD.encode(ciphertext),
        };
        Ok(serde_json::to_string_pretty(&sealed).expect("a purse file always serializes"))
    }

    /// Derives the key for `header` from `passphrase`.
    fn derive(header: Header, passphrase: &str) -> Result<Self, VaultError> {
        let kdf = &header.kdf;
        if kdf.algorithm != ALGORITHM {
            return Err(VaultError::Damaged("key derivation"));
        }
        let salt = hex::decode(&kdf.salt).map_err(|_| VaultError::Damaged("salt"))?;
        if salt.len() < argon2::MIN_SALT_LEN {
            return Err(VaultError::Damaged("salt"));
        }
        if kdf.memory_kib > MAX_MEMORY_KIB
            || kdf.iterations > MAX_ITERATIONS
            || kdf.parallelism > MAX_PARALLELISM
        {
            return Err(VaultError::Damaged("key derivation settings"));
        }
        let params = Params::new(kdf.memory_kib, kdf.iterations, kdf.parallelism, Some(32))
            .map_err(|_| VaultError::Damaged("key derivation settings"))?;
        // Allocated here rather than by the crate, so that running short of
        // memory is an error, never an abort, and the blocks, which the
        // passphrase fills, are zeroed when done.
        let mut blocks = Zeroizing::new(Vec::new());
        blocks
            .try_reserve_exact(params.block_count())
            .map_err(|_| VaultError::Memory)?;
        blocks.resize(params.block_count(), Block::default());
        let mut key = Zeroizing::new([0u8; 32]);
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password_into_with_memory(
                passphrase.as_bytes(),
                &salt,
                key.as_mut(),
                blocks.as_mut_slice(),
            )
            .map_err(|error| match error {
                argon2::Error::OutOfMemory | argon2::Error::MemoryTooLittle => VaultError::Memory,
                argon2::Error::PwdTooLong => VaultError::Passphrase,
                argon2::Error::SaltTooShort | argon2::Error::SaltTooLong => {
                    VaultError::Damaged("salt")
                }
                _ => VaultError::Damaged("key derivation settings"),
            })?;
        let cipher = XChaCha20Poly1305::new(&Key::from(*key));
        let aad = serde_json::to_vec(&header).expect("a header always serializes");
        Ok(Self {
            header,
            aad,
            cipher,
        })
    }

    /// Decrypts the ciphertext and reads the holder and mint list in it.
    fn decrypt(&self, nonce: &[u8; NONCE_LEN], ciphertext: &[u8]) -> Result<Contents, VaultError> {
        let payload = Payload {
            msg: ciphertext,
            aad: &self.aad,
        };
        let plain = Zeroizing::new(
            self.cipher
                .decrypt(&XNonce::from(*nonce), payload)
                .map_err(|_| VaultError::Passphrase)?,
        );
        let json = std::str::from_utf8(&plain).map_err(|_| VaultError::Damaged("contents"))?;
        let holder = Holder::from_json(json).map_err(VaultError::Holder)?;
        let listed: Listed =
            serde_json::from_str(json).map_err(|_| VaultError::Damaged("mint list"))?;
        Ok(Contents {
            holder,
            mints: listed.mints,
        })
    }
}

/// Reads a purse file's JSON, and decodes its nonce and ciphertext.
fn parse(text: &str) -> Result<(Sealed, [u8; NONCE_LEN], Vec<u8>), VaultError> {
    let probe: Probe = serde_json::from_str(text).map_err(|_| VaultError::NotSealed)?;
    if probe.format != FORMAT {
        return Err(VaultError::NotSealed);
    }
    if probe.version != VERSION {
        return Err(VaultError::Version(probe.version));
    }
    let sealed: Sealed = serde_json::from_str(text).map_err(|_| VaultError::Damaged("header"))?;
    let mut nonce = [0u8; NONCE_LEN];
    hex::decode_to_slice(&sealed.nonce, &mut nonce).map_err(|_| VaultError::Damaged("nonce"))?;
    let ciphertext = STANDARD
        .decode(&sealed.ciphertext)
        .map_err(|_| VaultError::Damaged("ciphertext"))?;
    Ok((sealed, nonce, ciphertext))
}

#[cfg(test)]
mod tests {
    use worgl::{Issuer, IssuerKey, LedgerConfig, Policy};

    use super::*;
    use crate::{MintInfo, MintView};

    const PASSPHRASE: &str = "correct horse battery staple";

    /// A purse `worgl` 0.1 wrote, with this passphrase: `worgl init`, then
    /// `worgl mint` of 12 in notes of at most 8, then `worgl request` of 5.
    const WRITTEN_BY_0_1: &str = include_str!("../tests/fixtures/purse-v1.worgl");

    /// The mint of that purse, which was new: four layers, the first of
    /// generation 0.
    const ITS_MINT: &str = "eefdebef624c7eca2dc7010746dfcb64a7e85459dfe31dd0068da35d208d3104";

    fn view(mint: &Issuer) -> MintView {
        MintView::new(&MintInfo {
            issuer: *mint.public_key(),
            layers: mint.layers(),
            policy: mint.policy().clone(),
            rotation: None,
            paused: false,
        })
        .unwrap()
    }

    /// A holder with notes of 8 and 4 from a new mint, and a request for 5
    /// open, as in the purse `worgl` 0.1 wrote.
    fn holder() -> (Issuer, Holder) {
        let ledger = LedgerConfig {
            layers: 4,
            layer_size: 1024,
        };
        let mut mint = Issuer::new(IssuerKey::generate(), ledger, Policy::cash_like()).unwrap();
        let mut holder = Holder::new();
        let request = holder.request_at_most(&view(&mint), 12, 8).unwrap();
        for output in &request.outputs {
            let note = mint.mint(output.amount, output.holder).unwrap();
            holder.absorb(&[note]);
        }
        holder.request(&view(&mint), 5).unwrap();
        (mint, holder)
    }

    /// `sealed` with `change` made to its JSON.
    fn altered(sealed: &str, change: impl FnOnce(&mut Value)) -> String {
        let mut file: Value = serde_json::from_str(sealed).unwrap();
        change(&mut file);
        serde_json::to_string_pretty(&file).unwrap()
    }

    #[test]
    fn a_sealed_holder_opens_with_its_mint_list() {
        let (mint, holder) = holder();
        let mut entry = MintEntry::new(*mint.public_key(), "https://mint.example.com");
        entry.extra.insert("name".into(), "Playground".into());
        let vault = Vault::new(PASSPHRASE).unwrap();
        let sealed = vault.seal(&holder, std::slice::from_ref(&entry)).unwrap();
        assert!(!sealed.contains("notes"), "{sealed}");

        let (again, opened) = Vault::open(&sealed, PASSPHRASE).unwrap();
        assert_eq!(opened.holder.to_json(), holder.to_json());
        assert_eq!(opened.mints, [entry.clone()]);
        assert_eq!(
            opened.server(mint.public_key()),
            Some("https://mint.example.com")
        );
        // The vault an open returns seals the same file again, and opens it
        // without deriving a key.
        let resealed = again.seal(&opened.holder, &opened.mints).unwrap();
        assert_ne!(resealed, sealed, "a fresh nonce for every seal");
        assert_eq!(vault.unseal(&resealed).unwrap().mints, [entry]);
        let unsealed = again.unseal(&sealed).unwrap();
        assert_eq!(unsealed.holder.to_json(), holder.to_json());
    }

    #[test]
    fn a_file_with_no_mint_list_is_what_worgl_0_1_wrote() {
        let (_, holder) = holder();
        let vault = Vault::new(PASSPHRASE).unwrap();
        let sealed = vault.seal(&holder, &[]).unwrap();
        // The same text, but for the random salt and nonce and what they
        // seal: members, their order, settings and indenting.
        let shape = |text: &str| {
            let mut shape = text.to_owned();
            for name in ["salt", "nonce", "ciphertext"] {
                let start = shape.find(&format!("\"{name}\": \"")).unwrap() + name.len() + 5;
                let end = start + shape[start..].find('"').unwrap();
                shape.replace_range(start..end, "");
            }
            shape
        };
        assert_eq!(shape(&sealed), shape(WRITTEN_BY_0_1));
        // What it seals is the holder's own JSON: a reader of a holder opens it.
        let (_, sealed_json) = open_plain(&sealed);
        assert_eq!(sealed_json, holder.to_json());
    }

    /// The plaintext of `sealed`, as the vault that opens it decrypts it.
    fn open_plain(sealed: &str) -> (Vault, String) {
        let (sealed, nonce, ciphertext) = parse(sealed).unwrap();
        let vault = Vault::derive(sealed.header, PASSPHRASE).unwrap();
        let payload = Payload {
            msg: &ciphertext,
            aad: &vault.aad,
        };
        let plain = vault.cipher.decrypt(&XNonce::from(nonce), payload).unwrap();
        (vault, String::from_utf8(plain).unwrap())
    }

    #[test]
    fn the_mint_list_rides_beside_the_holders_own_members() {
        let (mint, holder) = holder();
        let vault = Vault::new(PASSPHRASE).unwrap();
        let entry = MintEntry::new(*mint.public_key(), "https://mint.example.com");
        let sealed = vault.seal(&holder, &[entry]).unwrap();
        let (_, json) = open_plain(&sealed);
        // So a reader of a holder alone, as `worgl` 0.1 is, opens it.
        assert_eq!(
            Holder::from_json(&json).unwrap().to_json(),
            holder.to_json()
        );
        let plain: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            plain["mints"],
            serde_json::json!([{
                "mint": mint.public_key().to_string(),
                "server": "https://mint.example.com",
            }])
        );
        // No member of a holder's own is named so.
        let saved: Value = serde_json::from_str(&Holder::new().to_json()).unwrap();
        assert!(saved.get("mints").is_none(), "{saved}");
    }

    #[test]
    fn an_entrys_other_members_are_kept_but_never_its_own_twice() {
        let mint = IssuerKey::generate().public_key();
        let read: MintEntry = serde_json::from_value(serde_json::json!({
            "mint": mint.to_string(),
            "server": "https://a.example",
            "unit": "tokens",
        }))
        .unwrap();
        assert_eq!(read.extra["unit"], "tokens");
        let mut entry = read.clone();
        entry
            .extra
            .insert("server".into(), "https://b.example".into());
        let written = serde_json::to_string(&entry).unwrap();
        assert_eq!(
            written,
            format!(r#"{{"mint":"{mint}","server":"https://a.example","unit":"tokens"}}"#)
        );
    }

    #[test]
    fn a_file_worgl_0_1_wrote_opens() {
        let (vault, opened) = Vault::open(WRITTEN_BY_0_1, PASSPHRASE).unwrap();
        assert!(opened.mints.is_empty());
        let mint: PublicKey = ITS_MINT.parse().unwrap();
        assert_eq!(opened.holder.mints(), [mint]);
        let mut amounts: Vec<u64> = opened.holder.notes().map(|n| n.amount()).collect();
        amounts.sort_unstable();
        assert_eq!(amounts, [4, 8]);
        let view = MintView {
            issuer: mint,
            layers: 4,
            oldest: 0,
            policy: Policy::cash_like(),
            schedule: None,
            paused: false,
        };
        let balance = opened.holder.balance(&view);
        assert_eq!((balance.spendable, balance.requested), (12, 5));
        // Sealed again, with a mint list, it still opens with the same key.
        let entry = MintEntry::new(mint, "http://127.0.0.1:8080");
        let sealed = vault
            .seal(&opened.holder, std::slice::from_ref(&entry))
            .unwrap();
        assert_eq!(vault.unseal(&sealed).unwrap().mints, [entry]);
        let header = |text: &str| {
            let file: Value = serde_json::from_str(text).unwrap();
            (
                file["format"].clone(),
                file["version"].clone(),
                file["kdf"].clone(),
            )
        };
        assert_eq!(header(&sealed), header(WRITTEN_BY_0_1));
    }

    #[test]
    fn no_vault_seals_under_an_empty_passphrase() {
        assert!(matches!(Vault::new(""), Err(VaultError::EmptyPassphrase)));
        // A file sealed so, by a writer that allows it, still opens.
        let header = Header {
            format: FORMAT.into(),
            version: VERSION,
            kdf: Kdf {
                algorithm: ALGORITHM.into(),
                memory_kib: 1024,
                iterations: 1,
                parallelism: 1,
                salt: hex::encode([3u8; SALT_LEN]),
            },
        };
        let sealed = Vault::derive(header, "")
            .unwrap()
            .seal(&Holder::new(), &[])
            .unwrap();
        assert!(Vault::open(&sealed, "").is_ok());
        assert!(matches!(
            Vault::open(&sealed, "x"),
            Err(VaultError::Passphrase)
        ));
    }

    #[test]
    fn a_wrong_passphrase_opens_nothing() {
        assert!(matches!(
            Vault::open(WRITTEN_BY_0_1, "correct horse battery stapler"),
            Err(VaultError::Passphrase)
        ));
        // Nor does a vault of another salt, though the passphrase is right.
        let other = Vault::new(PASSPHRASE).unwrap();
        assert!(matches!(
            other.unseal(WRITTEN_BY_0_1),
            Err(VaultError::Passphrase)
        ));
    }

    #[test]
    fn an_altered_header_opens_nothing() {
        // Weaker settings, or another salt, do not open under the passphrase:
        // the header is authenticated with the ciphertext.
        let weakened = WRITTEN_BY_0_1.replace("\"iterations\": 3", "\"iterations\": 1");
        assert_ne!(weakened, WRITTEN_BY_0_1);
        assert!(matches!(
            Vault::open(&weakened, PASSPHRASE),
            Err(VaultError::Passphrase)
        ));
        let salted = altered(WRITTEN_BY_0_1, |file| {
            file["kdf"]["salt"] = "00".repeat(16).into();
        });
        assert!(matches!(
            Vault::open(&salted, PASSPHRASE),
            Err(VaultError::Passphrase)
        ));
        let renamed = altered(WRITTEN_BY_0_1, |file| {
            file["format"] = "worgl-holder".into()
        });
        assert!(matches!(
            Vault::open(&renamed, PASSPHRASE),
            Err(VaultError::NotSealed)
        ));
        let newer = altered(WRITTEN_BY_0_1, |file| file["version"] = 2.into());
        assert!(matches!(
            Vault::open(&newer, PASSPHRASE),
            Err(VaultError::Version(2))
        ));
        let other_kdf = altered(WRITTEN_BY_0_1, |file| {
            file["kdf"]["algorithm"] = "scrypt".into();
        });
        assert!(matches!(
            Vault::open(&other_kdf, PASSPHRASE),
            Err(VaultError::Damaged("key derivation"))
        ));
        let costly = altered(WRITTEN_BY_0_1, |file| {
            file["kdf"]["memory_kib"] = (MAX_MEMORY_KIB + 1).into();
        });
        assert!(matches!(
            Vault::open(&costly, PASSPHRASE),
            Err(VaultError::Damaged("key derivation settings"))
        ));
        let slow = altered(WRITTEN_BY_0_1, |file| {
            file["kdf"]["iterations"] = (MAX_ITERATIONS + 1).into();
        });
        assert!(matches!(
            Vault::open(&slow, PASSPHRASE),
            Err(VaultError::Damaged("key derivation settings"))
        ));
        let invalid = altered(WRITTEN_BY_0_1, |file| file["kdf"]["iterations"] = 0.into());
        assert!(matches!(
            Vault::open(&invalid, PASSPHRASE),
            Err(VaultError::Damaged("key derivation settings"))
        ));
        let short_salt = altered(WRITTEN_BY_0_1, |file| {
            file["kdf"]["salt"] = "0011".into();
        });
        assert!(matches!(
            Vault::open(&short_salt, PASSPHRASE),
            Err(VaultError::Damaged("salt"))
        ));
        let no_kdf = altered(WRITTEN_BY_0_1, |file| {
            file.as_object_mut().unwrap().remove("kdf");
        });
        assert!(matches!(
            Vault::open(&no_kdf, PASSPHRASE),
            Err(VaultError::Damaged("header"))
        ));
    }

    #[test]
    fn an_altered_ciphertext_opens_nothing() {
        let file: Value = serde_json::from_str(WRITTEN_BY_0_1).unwrap();
        let mut bytes = STANDARD
            .decode(file["ciphertext"].as_str().unwrap())
            .unwrap();
        bytes[10] ^= 1;
        let flipped = altered(WRITTEN_BY_0_1, |file| {
            file["ciphertext"] = STANDARD.encode(&bytes).into();
        });
        assert!(matches!(
            Vault::open(&flipped, PASSPHRASE),
            Err(VaultError::Passphrase)
        ));
        let cut = altered(WRITTEN_BY_0_1, |file| {
            let short = &file["ciphertext"].as_str().unwrap()[..40];
            file["ciphertext"] = short.to_owned().into();
        });
        assert!(matches!(
            Vault::open(&cut, PASSPHRASE),
            Err(VaultError::Passphrase)
        ));
        let renonced = altered(WRITTEN_BY_0_1, |file| {
            file["nonce"] = "11".repeat(24).into()
        });
        assert!(matches!(
            Vault::open(&renonced, PASSPHRASE),
            Err(VaultError::Passphrase)
        ));
        let not_base64 = altered(WRITTEN_BY_0_1, |file| file["ciphertext"] = "*".into());
        assert!(matches!(
            Vault::open(&not_base64, PASSPHRASE),
            Err(VaultError::Damaged("ciphertext"))
        ));
        let short_nonce = altered(WRITTEN_BY_0_1, |file| file["nonce"] = "11".into());
        assert!(matches!(
            Vault::open(&short_nonce, PASSPHRASE),
            Err(VaultError::Damaged("nonce"))
        ));
    }

    #[test]
    fn what_is_no_purse_file_is_refused_before_any_key_is_derived() {
        let (_, holder) = holder();
        for text in [holder.to_json().as_str(), "", "{}", "[1]", "not json"] {
            assert!(
                matches!(Vault::open(text, PASSPHRASE), Err(VaultError::NotSealed)),
                "{text}"
            );
        }
    }

    #[test]
    fn what_it_seals_must_be_a_holder() {
        let vault = Vault::new(PASSPHRASE).unwrap();
        let seal = |plain: &[u8]| {
            let nonce = [7u8; NONCE_LEN];
            let payload = Payload {
                msg: plain,
                aad: &vault.aad,
            };
            let ciphertext = vault.cipher.encrypt(&XNonce::from(nonce), payload).unwrap();
            serde_json::to_string(&Sealed {
                header: vault.header.clone(),
                nonce: hex::encode(nonce),
                ciphertext: STANDARD.encode(ciphertext),
            })
            .unwrap()
        };
        let newer = holder()
            .1
            .to_json()
            .replacen("\"format\":1", "\"format\":3", 1);
        assert!(matches!(
            vault.unseal(&seal(newer.as_bytes())),
            Err(VaultError::Holder(LoadError::Format(3)))
        ));
        assert!(matches!(
            vault.unseal(&seal(b"[]")),
            Err(VaultError::Holder(LoadError::Json(_)))
        ));
        assert!(matches!(
            vault.unseal(&seal(&[0xff, 0xfe])),
            Err(VaultError::Damaged("contents"))
        ));
        let listed = holder().1.to_json().replacen('{', r#"{"mints":7,"#, 1);
        assert!(matches!(
            vault.unseal(&seal(listed.as_bytes())),
            Err(VaultError::Damaged("mint list"))
        ));
    }

    #[test]
    fn settings_a_reader_accepts_open_though_no_vault_writes_them() {
        let (_, holder) = holder();
        let header = Header {
            format: FORMAT.into(),
            version: VERSION,
            kdf: Kdf {
                algorithm: ALGORITHM.into(),
                memory_kib: 1024,
                iterations: 1,
                parallelism: 2,
                salt: hex::encode([9u8; 8]),
            },
        };
        let vault = Vault::derive(header, PASSPHRASE).unwrap();
        let sealed = vault.seal(&holder, &[]).unwrap();
        let (_, opened) = Vault::open(&sealed, PASSPHRASE).unwrap();
        assert_eq!(opened.holder.to_json(), holder.to_json());
    }

    #[test]
    fn a_vault_prints_no_key() {
        let vault = Vault::new(PASSPHRASE).unwrap();
        let printed = format!("{vault:?}");
        assert!(printed.contains("argon2id"), "{printed}");
        assert!(!printed.contains("cipher"), "{printed}");
    }
}
