//! Banknotes.
//!
//! A note is a fixed 149-byte record signed by the issuer:
//!
//! | bytes    | field                                         |
//! |----------|-----------------------------------------------|
//! | 0        | format version, `1`                           |
//! | 1..33    | issuer public key                             |
//! | 33..41   | serial generation, big-endian `u64`           |
//! | 41..45   | serial index, big-endian `u32`                |
//! | 45..53   | amount, big-endian `u64`, never zero          |
//! | 53..85   | holder public key (one-time)                  |
//! | 85..149  | Ed25519 signature                             |
//!
//! The signature covers `b"worgl/note/v1"` followed by bytes 0..85, and is
//! made by the key of the note's generation, which the issuer key certifies
//! (see [`keyset`](crate::keyset)). The issuer stores no notes: the signature
//! makes a note self-authenticating, and the issuer's bit array says whether
//! it is still live. In text a note is the URL-safe, unpadded base64 of its
//! bytes.

use std::fmt;
use std::str::FromStr;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use thiserror::Error;

use crate::codec::{DecodeError, Reader, Writer};
use crate::keys::{PublicKey, Sign, Signature};
use crate::keyset::KeySet;
use crate::ledger::Serial;

const VERSION: u8 = 1;
const DOMAIN: &[u8] = b"worgl/note/v1";
const BODY_LEN: usize = 85;

/// A note could not be decoded or is not genuine.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum NoteError {
    /// The bytes or text are not a well-formed note.
    #[error("malformed note: {0}")]
    Malformed(#[from] DecodeError),
    /// The note was written in a format version this crate does not know.
    #[error("unknown note format version {0}")]
    Version(u8),
    /// The note names a different issuer.
    #[error("note was issued by a different issuer")]
    WrongIssuer,
    /// No key is known for the note's generation: it was dropped, or the
    /// key set predates it.
    #[error("no key is known for generation {0}")]
    UnknownGeneration(u64),
    /// The issuer's signature does not verify: the note is forged or altered.
    #[error("issuer signature does not verify")]
    BadSignature,
}

/// A banknote: an issuer-signed claim that `amount` belongs to whoever holds
/// the secret half of `holder`. See the [module documentation](self).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Note {
    issuer: PublicKey,
    serial: Serial,
    amount: u64,
    holder: PublicKey,
    signature: Signature,
}

impl Note {
    /// Length of a note in bytes.
    pub const LEN: usize = BODY_LEN + 64;

    pub(crate) fn issue(
        issuer: PublicKey,
        key: &dyn Sign,
        serial: Serial,
        amount: u64,
        holder: PublicKey,
    ) -> Self {
        debug_assert!(amount > 0);
        let mut note = Self {
            issuer,
            serial,
            amount,
            holder,
            signature: Signature::from_bytes(&[0; 64]),
        };
        note.signature = key.sign(&note.signed_message());
        note
    }

    /// The issuer key of the issuer that made the note.
    pub fn issuer(&self) -> &PublicKey {
        &self.issuer
    }

    /// The note's serial number.
    pub fn serial(&self) -> Serial {
        self.serial
    }

    /// Face value. Always positive.
    pub fn amount(&self) -> u64 {
        self.amount
    }

    /// The one-time key whose holder may spend the note.
    pub fn holder(&self) -> &PublicKey {
        &self.holder
    }

    /// The issuer's signature.
    pub fn signature(&self) -> &Signature {
        &self.signature
    }

    /// Checks that the issuer of `keys` made this note, signed by the key of
    /// its generation. Says nothing about whether it is still live; only the
    /// issuer's ledger knows that.
    pub fn verify(&self, keys: &KeySet) -> Result<(), NoteError> {
        if &self.issuer != keys.issuer() {
            return Err(NoteError::WrongIssuer);
        }
        let generation = self.serial.generation;
        let key = keys
            .signer(generation)
            .ok_or(NoteError::UnknownGeneration(generation))?;
        if !key.verify(&self.signed_message(), &self.signature) {
            return Err(NoteError::BadSignature);
        }
        Ok(())
    }

    /// The note's 149-byte encoding.
    pub fn to_bytes(&self) -> [u8; Self::LEN] {
        let mut w = Writer::with_capacity(Self::LEN);
        self.write_body(&mut w);
        w.bytes(&self.signature.to_bytes());
        w.finish()
            .try_into()
            .expect("a note encodes to exactly Note::LEN bytes")
    }

    /// Decodes a note. The result is well formed but unverified: call
    /// [`verify`](Self::verify) before trusting it.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, NoteError> {
        let mut r = Reader::new(bytes);
        let version = r.u8()?;
        if version != VERSION {
            return Err(NoteError::Version(version));
        }
        let issuer = read_key(&mut r)?;
        let serial = Serial::new(r.u64()?, r.u32()?);
        let amount = r.u64()?;
        if amount == 0 {
            return Err(DecodeError::Invalid("amount").into());
        }
        let holder = read_key(&mut r)?;
        let signature = Signature::from_bytes(&r.array()?);
        r.finish()?;
        Ok(Self {
            issuer,
            serial,
            amount,
            holder,
            signature,
        })
    }

    fn write_body(&self, w: &mut Writer) {
        w.u8(VERSION)
            .bytes(self.issuer.as_bytes())
            .u64(self.serial.generation)
            .u32(self.serial.index)
            .u64(self.amount)
            .bytes(self.holder.as_bytes());
    }

    fn signed_message(&self) -> Vec<u8> {
        let mut w = Writer::with_capacity(DOMAIN.len() + BODY_LEN);
        w.bytes(DOMAIN);
        self.write_body(&mut w);
        w.finish()
    }
}

pub(crate) fn read_key(r: &mut Reader<'_>) -> Result<PublicKey, DecodeError> {
    PublicKey::from_bytes(&r.array()?).map_err(|_| DecodeError::Invalid("public key"))
}

impl fmt::Debug for Note {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Note")
            .field("serial", &self.serial)
            .field("amount", &self.amount)
            .field("holder", &self.holder)
            .field("issuer", &self.issuer)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for Note {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&URL_SAFE_NO_PAD.encode(self.to_bytes()))
    }
}

impl FromStr for Note {
    type Err = NoteError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = URL_SAFE_NO_PAD
            .decode(s.trim())
            .map_err(|_| DecodeError::Invalid("base64"))?;
        Self::from_bytes(&bytes)
    }
}

mod serde_impls {
    use std::fmt;

    use serde::de::{self, Visitor};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::Note;

    impl Serialize for Note {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            if s.is_human_readable() {
                s.collect_str(self)
            } else {
                s.serialize_bytes(&self.to_bytes())
            }
        }
    }

    struct NoteVisitor;

    impl Visitor<'_> for NoteVisitor {
        type Value = Note;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a note as unpadded URL-safe base64, or its 149 bytes")
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<Note, E> {
            v.parse().map_err(E::custom)
        }

        fn visit_bytes<E: de::Error>(self, v: &[u8]) -> Result<Note, E> {
            Note::from_bytes(v).map_err(E::custom)
        }
    }

    impl<'de> Deserialize<'de> for Note {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            if d.is_human_readable() {
                d.deserialize_str(NoteVisitor)
            } else {
                d.deserialize_bytes(NoteVisitor)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{GenerationKey, HolderKey, IssuerKey};
    use crate::keyset::KeyCertificate;

    /// An issuer whose generation 7 has a key of its own, and a note from it.
    fn sample() -> (KeySet, Note) {
        let issuer = IssuerKey::generate();
        let key = GenerationKey::generate();
        let certificate = KeyCertificate::sign(&issuer, 7, key.public_key());
        let keys = KeySet::new(issuer.public_key(), 6, vec![certificate]).unwrap();
        let holder = HolderKey::generate().public_key();
        let note = Note::issue(issuer.public_key(), &key, Serial::new(7, 42), 1000, holder);
        (keys, note)
    }

    #[test]
    fn issued_notes_verify_against_their_issuer_only() {
        let (keys, note) = sample();
        assert_eq!(note.verify(&keys), Ok(()));
        let stranger = KeySet::new(IssuerKey::generate().public_key(), 6, Vec::new()).unwrap();
        assert_eq!(note.verify(&stranger), Err(NoteError::WrongIssuer));
    }

    #[test]
    fn notes_verify_under_their_own_generations_key() {
        let (keys, note) = sample();
        // Generation 6 is signed by the issuer key, so the generation key's
        // signature does not pass there.
        let mut bytes = note.to_bytes();
        bytes[33..41].copy_from_slice(&6u64.to_be_bytes());
        let moved = Note::from_bytes(&bytes).unwrap();
        assert_eq!(moved.verify(&keys), Err(NoteError::BadSignature));
        // Generation 8 has no key yet, and generation 5 was dropped.
        for generation in [5u64, 8] {
            bytes[33..41].copy_from_slice(&generation.to_be_bytes());
            let note = Note::from_bytes(&bytes).unwrap();
            assert_eq!(
                note.verify(&keys),
                Err(NoteError::UnknownGeneration(generation))
            );
        }
    }

    #[test]
    fn bytes_and_text_round_trip() {
        let (_, note) = sample();
        let bytes = note.to_bytes();
        assert_eq!(bytes.len(), Note::LEN);
        assert_eq!(Note::from_bytes(&bytes), Ok(note));
        assert_eq!(note.to_string().parse(), Ok(note));
        let json = serde_json::to_string(&note).unwrap();
        assert_eq!(serde_json::from_str::<Note>(&json).unwrap(), note);
    }

    #[test]
    fn any_altered_field_breaks_the_signature() {
        let (keys, note) = sample();
        // Flip one bit in each of serial, amount and signature.
        for offset in [44, 52, 100] {
            let mut bytes = note.to_bytes();
            bytes[offset] ^= 1;
            let altered = Note::from_bytes(&bytes).unwrap();
            assert_eq!(altered.verify(&keys), Err(NoteError::BadSignature));
        }
    }

    #[test]
    fn rejects_malformed_input() {
        let (_, note) = sample();
        let bytes = note.to_bytes();
        assert!(matches!(
            Note::from_bytes(&bytes[..100]),
            Err(NoteError::Malformed(DecodeError::Truncated))
        ));
        let mut v2 = bytes;
        v2[0] = 2;
        assert_eq!(Note::from_bytes(&v2), Err(NoteError::Version(2)));
        let mut zero = bytes;
        zero[45..53].fill(0);
        assert!(matches!(
            Note::from_bytes(&zero),
            Err(NoteError::Malformed(_))
        ));
        assert!("not base64!".parse::<Note>().is_err());
    }
}
