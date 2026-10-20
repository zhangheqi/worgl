//! Generation keys: each generation's notes are signed by a key of its own.
//!
//! An issuer is known by its [`IssuerKey`], whose public half every note
//! names. It does not sign notes. At every rotation the issuer makes a fresh
//! [`GenerationKey`] for the new last layer and certifies it with the issuer
//! key over
//!
//! ```text
//! b"worgl/generation-key/v1" ‖ issuer key (32) ‖ generation (u64) ‖ generation key (32)
//! ```
//!
//! and every note of that generation is signed with it. The generation key's
//! secret is erased once its generation is dropped, when the last of its
//! notes expires. So a generation key that leaks forges notes for at most as
//! many rotations as there are layers, and keys made after the leak are not
//! exposed by it.
//!
//! The issuer checks notes only against the keys it made itself, so a leaked
//! issuer key cannot get a forged note past the issuer in a generation that
//! has its own key. It can certify a false key, though, and fool someone who
//! checks notes offline with a key set it serves them.
//!
//! A [`KeySet`] holds the public halves and certificates for the live
//! generations, which is all anyone needs to check a note's signature
//! offline. A [`KeyRing`] holds the secrets, which only the issuer needs; a
//! [`store`](crate::store) keeps it in a file of its own, never in the
//! snapshot or the log. The issuer also keeps the public halves of the last
//! generations dropped, one fewer than it has layers, so that a request
//! resent after its inputs' generation was dropped still verifies (see
//! [`Verifier`](crate::Verifier)).
//!
//! Issuers created before generation keys existed signed every note with the
//! issuer key. Their generations keep verifying under it just as a
//! generation with a key of its own would: while live, and for a resent
//! request while dropped recently enough that its key would still be kept.
//! Every generation created since has a key of its own.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::codec::{DecodeError, Reader, Writer};
use crate::error::SetupError;
use crate::keys::{GenerationKey, IssuerKey, PublicKey, Sign, Signature};
use crate::note::read_key;

const DOMAIN: &[u8] = b"worgl/generation-key/v1";
const RING_MAGIC: &[u8; 8] = b"WORGLKEY";
const RING_VERSION: u16 = 1;

/// The issuer key's statement that `key` signs the notes of `generation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct KeyCertificate {
    /// The generation whose notes the key signs.
    pub generation: u64,
    /// The generation key's public half.
    pub key: PublicKey,
    /// The issuer key's signature over the statement.
    pub signature: Signature,
}

impl KeyCertificate {
    pub(crate) fn sign(issuer: &IssuerKey, generation: u64, key: PublicKey) -> Self {
        let signature = issuer.sign(&message(&issuer.public_key(), generation, &key));
        Self {
            generation,
            key,
            signature,
        }
    }

    /// Whether the issuer key `issuer` made this certificate.
    pub fn verify(&self, issuer: &PublicKey) -> bool {
        issuer.verify(
            &message(issuer, self.generation, &self.key),
            &self.signature,
        )
    }

    pub(crate) fn write(&self, w: &mut Writer) {
        w.u64(self.generation)
            .bytes(self.key.as_bytes())
            .bytes(&self.signature.to_bytes());
    }

    pub(crate) fn read(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            generation: r.u64()?,
            key: read_key(r)?,
            signature: Signature::from_bytes(&r.array()?),
        })
    }
}

fn message(issuer: &PublicKey, generation: u64, key: &PublicKey) -> Vec<u8> {
    let mut w = Writer::with_capacity(DOMAIN.len() + 72);
    w.bytes(DOMAIN)
        .bytes(issuer.as_bytes())
        .u64(generation)
        .bytes(key.as_bytes());
    w.finish()
}

/// The public keys that sign an issuer's live generations.
///
/// Generations from `oldest` up to the first certified one, if any, are
/// signed by the issuer key itself; each later one by the key its
/// certificate names. Certificates are checked whenever a set is built or
/// deserialized, so a `KeySet` only ever holds keys its issuer certified.
/// Check that [`issuer`](Self::issuer) is the issuer you expect before
/// trusting notes with it.
///
/// The reference server serves a mint's set at `GET /v1/mints/{mint}/keys`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawKeySet", into = "RawKeySet")]
pub struct KeySet {
    issuer: PublicKey,
    oldest: u64,
    keys: Arc<[KeyCertificate]>,
}

#[derive(Serialize, Deserialize)]
struct RawKeySet {
    issuer: PublicKey,
    oldest: u64,
    keys: Vec<KeyCertificate>,
}

impl TryFrom<RawKeySet> for KeySet {
    type Error = DecodeError;

    fn try_from(raw: RawKeySet) -> Result<Self, DecodeError> {
        Self::new(raw.issuer, raw.oldest, raw.keys)
    }
}

impl From<KeySet> for RawKeySet {
    fn from(set: KeySet) -> Self {
        Self {
            issuer: set.issuer,
            oldest: set.oldest,
            keys: set.keys.to_vec(),
        }
    }
}

impl KeySet {
    /// Checks that the certificates are the issuer's, for consecutive
    /// generations, none older than `oldest`.
    pub(crate) fn new(
        issuer: PublicKey,
        oldest: u64,
        keys: Vec<KeyCertificate>,
    ) -> Result<Self, DecodeError> {
        let consecutive = keys
            .windows(2)
            .all(|pair| pair[0].generation.checked_add(1) == Some(pair[1].generation));
        let in_range = keys.first().is_none_or(|first| first.generation >= oldest);
        let mut seen = BTreeSet::new();
        let distinct = keys.iter().all(|c| c.key != issuer && seen.insert(c.key));
        if !consecutive || !in_range || !distinct || !keys.iter().all(|c| c.verify(&issuer)) {
            return Err(DecodeError::Invalid("generation keys"));
        }
        Ok(Self {
            issuer,
            oldest,
            keys: keys.into(),
        })
    }

    /// The issuer key, which every note names.
    pub fn issuer(&self) -> &PublicKey {
        &self.issuer
    }

    /// The oldest live generation. Older notes have expired.
    pub fn oldest(&self) -> u64 {
        self.oldest
    }

    /// The certificates, oldest generation first.
    pub fn certificates(&self) -> &[KeyCertificate] {
        &self.keys
    }

    /// The key that signs `generation`'s notes, or `None` for a generation
    /// that has been dropped or that this set does not know yet.
    pub fn signer(&self, generation: u64) -> Option<&PublicKey> {
        if generation < self.oldest {
            return None;
        }
        match self.keys.first() {
            Some(first) if generation >= first.generation => self
                .keys
                .get(usize::try_from(generation - first.generation).ok()?)
                .map(|c| &c.key),
            _ => Some(&self.issuer),
        }
    }

    /// The first generation with a key of its own, if any.
    pub(crate) fn first_keyed(&self) -> Option<u64> {
        self.keys.first().map(|c| c.generation)
    }

    /// The set from generation `oldest` on, followed by `created`: the key a
    /// rotation certified, or `None` for a rotation made before generation
    /// keys existed or for no rotation at all.
    pub(crate) fn rotated(&self, oldest: u64, created: Option<KeyCertificate>) -> Self {
        let mut keys: Vec<KeyCertificate> = self
            .keys
            .iter()
            .filter(|c| c.generation >= oldest)
            .copied()
            .collect();
        keys.extend(created);
        Self {
            issuer: self.issuer,
            oldest,
            keys: keys.into(),
        }
    }
}

/// The secret halves of an issuer's generation keys.
///
/// It is the one part of an issuer's state that is secret besides the issuer
/// key, so it is kept apart from snapshots and logs; see
/// [`Issuer::key_ring`](crate::Issuer::key_ring). Its encoding is
///
/// ```text
/// b"WORGLKEY" ‖ version (u16) ‖ issuer key (32) ‖ count (u32)
///   ‖ secret (32) × count ‖ SHA-256 of everything before it (32)
/// ```
#[derive(Clone)]
pub struct KeyRing {
    issuer: PublicKey,
    keys: BTreeMap<PublicKey, GenerationKey>,
}

impl KeyRing {
    /// An empty ring for the issuer key `issuer`.
    pub fn new(issuer: PublicKey) -> Self {
        Self {
            issuer,
            keys: BTreeMap::new(),
        }
    }

    /// The issuer key the ring belongs to.
    pub fn issuer(&self) -> &PublicKey {
        &self.issuer
    }

    /// Number of keys held.
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Whether the ring holds no keys.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Adds a key.
    pub fn insert(&mut self, key: GenerationKey) {
        self.keys.insert(key.public_key(), key);
    }

    pub(crate) fn get(&self, public: &PublicKey) -> Option<&GenerationKey> {
        self.keys.get(public)
    }

    /// Adds every key of `other`.
    pub fn extend(&mut self, other: &KeyRing) {
        for (public, key) in &other.keys {
            self.keys.entry(*public).or_insert_with(|| key.clone());
        }
    }

    pub(crate) fn remove(&mut self, public: &PublicKey) {
        self.keys.remove(public);
    }

    /// Erases every key `keep` rejects.
    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&PublicKey) -> bool) {
        self.keys.retain(|public, _| keep(public));
    }

    /// The ring's encoding. It holds secrets: zero it once written.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = Writer::with_capacity(8 + 2 + 32 + 4 + 32 * self.keys.len() + 32);
        w.bytes(RING_MAGIC)
            .u16(RING_VERSION)
            .bytes(self.issuer.as_bytes())
            .u32(self.keys.len() as u32);
        for key in self.keys.values() {
            let mut secret = key.to_secret_bytes();
            w.bytes(&secret);
            secret.fill(0);
        }
        let mut bytes = w.finish();
        let checksum: [u8; 32] = Sha256::digest(&bytes).into();
        bytes.extend_from_slice(&checksum);
        bytes
    }

    /// Decodes a ring written by [`to_bytes`](Self::to_bytes).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SetupError> {
        let split = bytes.len().checked_sub(32).ok_or(DecodeError::Truncated)?;
        let (body, checksum) = bytes.split_at(split);
        let expected: [u8; 32] = Sha256::digest(body).into();
        if checksum != expected {
            return Err(SetupError::Checksum);
        }
        let mut r = Reader::new(body);
        if &r.array::<8>()? != RING_MAGIC || r.u16()? != RING_VERSION {
            return Err(DecodeError::Invalid("key ring header").into());
        }
        let mut ring = Self::new(read_key(&mut r)?);
        for _ in 0..r.u32()? {
            let mut secret = r.array::<32>()?;
            ring.insert(GenerationKey::from_secret_bytes(&secret));
            secret.fill(0);
        }
        r.finish()?;
        Ok(ring)
    }
}

impl std::fmt::Debug for KeyRing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyRing")
            .field("issuer", &self.issuer)
            .field("keys", &self.keys.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// Whichever key signs one generation's notes.
pub(crate) fn signing_key<'a>(
    issuer: &'a IssuerKey,
    keys: &KeySet,
    ring: &'a KeyRing,
    generation: u64,
) -> Option<&'a dyn Sign> {
    match keys.first_keyed() {
        Some(first) if generation >= first => {
            let public = keys.signer(generation)?;
            ring.get(public).map(|key| key as &dyn Sign)
        }
        _ => Some(issuer),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn certificates(issuer: &IssuerKey, generations: std::ops::Range<u64>) -> Vec<KeyCertificate> {
        generations
            .map(|g| KeyCertificate::sign(issuer, g, GenerationKey::generate().public_key()))
            .collect()
    }

    #[test]
    fn a_set_names_the_issuer_key_below_its_first_certificate() {
        let issuer = IssuerKey::generate();
        let public = issuer.public_key();
        let keys = certificates(&issuer, 5..8);
        let set = KeySet::new(public, 3, keys.clone()).unwrap();
        assert_eq!(set.signer(2), None);
        assert_eq!(set.signer(3), Some(&public));
        assert_eq!(set.signer(4), Some(&public));
        assert_eq!(set.signer(5), Some(&keys[0].key));
        assert_eq!(set.signer(7), Some(&keys[2].key));
        assert_eq!(set.signer(8), None);
        assert_eq!(set.signer(u64::MAX), None);

        // A rotation drops generation 3 and certifies 8.
        let next = KeyCertificate::sign(&issuer, 8, GenerationKey::generate().public_key());
        let rotated = set.rotated(4, Some(next));
        assert_eq!(rotated.signer(3), None);
        assert_eq!(rotated.signer(8), Some(&next.key));
        assert_eq!(rotated.certificates().len(), 4);
    }

    #[test]
    fn only_the_issuers_consecutive_certificates_make_a_set() {
        let issuer = IssuerKey::generate();
        let public = issuer.public_key();
        let mut gap = certificates(&issuer, 0..2);
        gap.extend(certificates(&issuer, 3..4));
        assert!(KeySet::new(public, 0, gap).is_err());
        assert!(KeySet::new(public, 1, certificates(&issuer, 0..2)).is_err());
        let stranger = IssuerKey::generate();
        assert!(KeySet::new(public, 0, certificates(&stranger, 0..2)).is_err());

        // One key may not sign two generations.
        let key = GenerationKey::generate().public_key();
        let twice = (0..2)
            .map(|g| KeyCertificate::sign(&issuer, g, key))
            .collect();
        assert!(KeySet::new(public, 0, twice).is_err());

        let mut forged = certificates(&issuer, 0..2);
        forged[1].key = GenerationKey::generate().public_key();
        assert!(KeySet::new(public, 0, forged.clone()).is_err());

        // Deserializing checks the same way.
        let set = KeySet::new(public, 0, certificates(&issuer, 0..2)).unwrap();
        let json = serde_json::to_value(&set).unwrap();
        assert_eq!(serde_json::from_value::<KeySet>(json.clone()).unwrap(), set);
        let mut tampered = json;
        tampered["keys"][1]["key"] = serde_json::to_value(forged[1].key).unwrap();
        assert!(serde_json::from_value::<KeySet>(tampered).is_err());
    }

    #[test]
    fn a_ring_round_trips_and_detects_corruption() {
        let issuer = IssuerKey::generate().public_key();
        let mut ring = KeyRing::new(issuer);
        let keys: Vec<GenerationKey> = (0..3).map(|_| GenerationKey::generate()).collect();
        for key in &keys {
            ring.insert(key.clone());
        }
        let bytes = ring.to_bytes();
        let again = KeyRing::from_bytes(&bytes).unwrap();
        assert_eq!(again.issuer(), &issuer);
        assert_eq!(again.len(), 3);
        for key in &keys {
            let found = again.get(&key.public_key()).unwrap();
            assert_eq!(found.to_secret_bytes(), key.to_secret_bytes());
        }

        let mut corrupt = bytes.clone();
        corrupt[50] ^= 1;
        assert!(matches!(
            KeyRing::from_bytes(&corrupt),
            Err(SetupError::Checksum)
        ));
        assert!(KeyRing::from_bytes(&bytes[..10]).is_err());

        // Debug shows public keys only.
        let debug = format!("{ring:?}");
        assert!(!debug.contains(&hex::encode(keys[0].to_secret_bytes())));
        assert!(debug.contains(&keys[0].public_key().to_string()));
    }
}
