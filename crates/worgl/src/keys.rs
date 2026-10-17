//! Ed25519 keys and signatures.
//!
//! An issuer is known by its [`IssuerKey`], whose public half every note
//! names. Each generation's notes are signed by a [`GenerationKey`] of its
//! own, which the issuer key certifies (see [`keyset`](crate::keyset)).
//! Whoever is to be paid makes a fresh [`HolderKey`] and hands out only its
//! [`PublicKey`]; the note they receive names that key, and spending the note
//! needs a signature from it. A holder key is meant to be used for exactly one
//! note, so nothing at the issuer ties two notes to the same holder. A
//! [`MintKey`] belongs to a mint authority, whose signature the issuer
//! requires before it mints.
//!
//! Keys and signatures travel as lowercase hex in human-readable formats such
//! as JSON, and as raw bytes in binary ones.

use std::fmt;
use std::str::FromStr;

use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use thiserror::Error;

use crate::hex_serde::parse_hex;

/// Length in bytes of a public key.
pub const PUBLIC_KEY_LEN: usize = 32;
/// Length in bytes of a secret key.
pub const SECRET_KEY_LEN: usize = 32;
/// Length in bytes of a signature.
pub const SIGNATURE_LEN: usize = 64;

/// A key or signature could not be parsed or created.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum KeyError {
    /// The text was not hex of the expected length.
    #[error("expected {0} bytes of hex")]
    Hex(usize),
    /// The bytes are not a valid Ed25519 public key.
    #[error("not a valid Ed25519 public key")]
    InvalidPoint,
    /// The key has small order, so signatures under it prove nothing.
    #[error("weak Ed25519 public key")]
    Weak,
    /// The operating system's random number generator failed.
    #[error("the system random number generator is unavailable")]
    Rng,
}

/// An Ed25519 public key: the issuer's, or the one-time key a note is bound to.
///
/// Construction rejects encodings that are not points on the curve and the
/// small-order ("weak") keys for which a signature does not bind the signer.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PublicKey([u8; PUBLIC_KEY_LEN]);

impl PublicKey {
    /// Parses a public key from its 32-byte encoding.
    pub fn from_bytes(bytes: &[u8; PUBLIC_KEY_LEN]) -> Result<Self, KeyError> {
        let key = VerifyingKey::from_bytes(bytes).map_err(|_| KeyError::InvalidPoint)?;
        if key.is_weak() {
            return Err(KeyError::Weak);
        }
        Ok(Self(*bytes))
    }

    /// The 32-byte encoding.
    pub fn to_bytes(&self) -> [u8; PUBLIC_KEY_LEN] {
        self.0
    }

    /// The 32-byte encoding, borrowed.
    pub fn as_bytes(&self) -> &[u8; PUBLIC_KEY_LEN] {
        &self.0
    }

    /// Strict Ed25519 verification: rejects malleable and non-canonical signatures.
    pub(crate) fn verify(&self, message: &[u8], signature: &Signature) -> bool {
        // The bytes were validated on construction, so this cannot fail.
        let Ok(key) = VerifyingKey::from_bytes(&self.0) else {
            return false;
        };
        let signature = ed25519_dalek::Signature::from_bytes(&signature.0);
        key.verify_strict(message, &signature).is_ok()
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey({self})")
    }
}

impl FromStr for PublicKey {
    type Err = KeyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_bytes(&parse_hex(s).ok_or(KeyError::Hex(PUBLIC_KEY_LEN))?)
    }
}

/// An Ed25519 signature.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Signature([u8; SIGNATURE_LEN]);

impl Signature {
    /// Wraps a 64-byte signature. Validity is only known once it is verified.
    pub fn from_bytes(bytes: &[u8; SIGNATURE_LEN]) -> Self {
        Self(*bytes)
    }

    /// The 64-byte encoding.
    pub fn to_bytes(&self) -> [u8; SIGNATURE_LEN] {
        self.0
    }
}

impl fmt::Display for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

impl fmt::Debug for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Signature({self})")
    }
}

impl FromStr for Signature {
    type Err = KeyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(parse_hex(s).ok_or(KeyError::Hex(SIGNATURE_LEN))?))
    }
}

/// Any of the secret key types, for code that signs with whichever one it is handed.
pub(crate) trait Sign {
    fn sign(&self, message: &[u8]) -> Signature;
}

macro_rules! secret_key {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        ///
        /// The secret is zeroed when the key is dropped, and `Debug` prints
        /// only the public half.
        #[derive(Clone)]
        pub struct $name(SigningKey);

        impl $name {
            /// Generates a key from the operating system's random number generator.
            ///
            /// # Panics
            ///
            /// If the system random number generator fails; see
            /// [`try_generate`](Self::try_generate) to handle that instead.
            pub fn generate() -> Self {
                Self::try_generate().expect("system random number generator failed")
            }

            /// Generates a key, or reports that the system random number generator failed.
            pub fn try_generate() -> Result<Self, KeyError> {
                let mut secret = [0u8; SECRET_KEY_LEN];
                getrandom::fill(&mut secret).map_err(|_| KeyError::Rng)?;
                let key = Self::from_secret_bytes(&secret);
                secret.fill(0);
                Ok(key)
            }

            /// Rebuilds a key from its 32-byte secret.
            pub fn from_secret_bytes(secret: &[u8; SECRET_KEY_LEN]) -> Self {
                Self(SigningKey::from_bytes(secret))
            }

            /// The 32-byte secret. Keep it out of logs and storage you do not control.
            pub fn to_secret_bytes(&self) -> [u8; SECRET_KEY_LEN] {
                self.0.to_bytes()
            }

            /// The public half.
            pub fn public_key(&self) -> PublicKey {
                PublicKey(self.0.verifying_key().to_bytes())
            }

            pub(crate) fn sign(&self, message: &[u8]) -> Signature {
                Signature(self.0.sign(message).to_bytes())
            }
        }

        impl Sign for $name {
            fn sign(&self, message: &[u8]) -> Signature {
                $name::sign(self, message)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_struct(stringify!($name))
                    .field("public", &self.public_key())
                    .finish_non_exhaustive()
            }
        }
    };
}

secret_key!(
    /// The issuer's identity key. Every note names its public half.
    ///
    /// It certifies the [`GenerationKey`] that signs each generation's notes,
    /// and signs notes itself only in generations created before generation
    /// keys existed. Serial indexes are also permuted under a key derived
    /// from it.
    IssuerKey
);

secret_key!(
    /// The key that signs one generation's notes, certified by the
    /// [`IssuerKey`]. The issuer makes a new one at every rotation and erases
    /// it once its generation is dropped, so a leaked generation key forges
    /// notes only until then.
    GenerationKey
);

secret_key!(
    /// A mint authority's key. An issuer mints on request only for the keys
    /// its policy lists; see [`mint`](crate::mint).
    MintKey
);

secret_key!(
    /// A one-time key that a note is bound to. Its holder alone can spend the note.
    ///
    /// Make a fresh one for every note you expect to receive: reusing a holder
    /// key tells the issuer that two notes share an owner.
    HolderKey
);

mod serde_impls {
    use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

    use super::{PublicKey, Signature};
    use crate::hex_serde;

    impl Serialize for PublicKey {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            hex_serde::serialize(&self.0, s)
        }
    }

    impl<'de> Deserialize<'de> for PublicKey {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            PublicKey::from_bytes(&hex_serde::deserialize(d)?).map_err(de::Error::custom)
        }
    }

    impl Serialize for Signature {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            hex_serde::serialize(&self.0, s)
        }
    }

    impl<'de> Deserialize<'de> for Signature {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            Ok(Signature(hex_serde::deserialize(d)?))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_verify_only_for_the_signed_message_and_key() {
        let key = HolderKey::generate();
        let other = HolderKey::generate();
        let sig = key.sign(b"pay bob");
        assert!(key.public_key().verify(b"pay bob", &sig));
        assert!(!key.public_key().verify(b"pay eve", &sig));
        assert!(!other.public_key().verify(b"pay bob", &sig));
    }

    #[test]
    fn secret_round_trip_keeps_the_public_key() {
        let key = IssuerKey::generate();
        let again = IssuerKey::from_secret_bytes(&key.to_secret_bytes());
        assert_eq!(key.public_key(), again.public_key());
    }

    #[test]
    fn rejects_weak_and_invalid_public_keys() {
        // The identity point has small order.
        let mut identity = [0u8; 32];
        identity[0] = 1;
        assert_eq!(PublicKey::from_bytes(&identity), Err(KeyError::Weak));
        // y = 2 is not the y-coordinate of any point on the curve.
        let mut off_curve = [0u8; 32];
        off_curve[0] = 2;
        assert_eq!(
            PublicKey::from_bytes(&off_curve),
            Err(KeyError::InvalidPoint)
        );
    }

    #[test]
    fn hex_text_round_trips() {
        let public = HolderKey::generate().public_key();
        assert_eq!(public.to_string().parse::<PublicKey>(), Ok(public));
        assert_eq!("abcd".parse::<PublicKey>(), Err(KeyError::Hex(32)));
        let json = serde_json::to_string(&public).unwrap();
        assert_eq!(json, format!("\"{public}\""));
        assert_eq!(serde_json::from_str::<PublicKey>(&json).unwrap(), public);
    }

    #[test]
    fn debug_never_prints_the_secret() {
        let key = HolderKey::from_secret_bytes(&[7u8; 32]);
        let debug = format!("{key:?}");
        assert!(!debug.contains(&hex::encode([7u8; 32])));
        assert!(debug.contains(&key.public_key().to_string()));
    }
}
