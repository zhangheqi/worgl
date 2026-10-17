//! Transfer requests: what a holder signs to hand notes on.
//!
//! A transfer consumes input notes and asks the issuer for new notes, bound
//! to the receivers' one-time keys. The holder of every input signs one
//! message that names all inputs and all outputs:
//!
//! ```text
//! b"worgl/transfer/v1" ‖ issuer key (32)
//!   ‖ input count (u32) ‖ for each input:  generation (u64) ‖ index (u32)
//!   ‖ output count (u32) ‖ for each output: amount (u64) ‖ holder key (32)
//! ```
//!
//! All integers are big-endian. Because the message names the outputs, nobody
//! who sees a request in flight can redirect the money, and because it names
//! every input, signatures cannot be moved between requests. Anyone may submit
//! a signed request: the payer, or the payee the payer handed it to.
//!
//! The SHA-256 of the message is the request's [`TransferId`]. The issuer
//! uses it to recognise a resent request and answer it with the same notes.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

use crate::Error;
use crate::codec::Writer;
use crate::hex_serde;
use crate::keys::{HolderKey, PublicKey, Signature};
use crate::ledger::Serial;
use crate::note::Note;
use crate::policy::Policy;

const DOMAIN: &[u8] = b"worgl/transfer/v1";

/// A note to be created by a transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Output {
    /// Face value of the new note.
    pub amount: u64,
    /// The receiver's fresh one-time key.
    pub holder: PublicKey,
}

/// An input note together with its holder's signature over the transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spend {
    /// The note being spent.
    pub note: Note,
    /// Signature by the note's holder key over the transfer message.
    pub signature: Signature,
}

/// A signed request to consume `inputs` and issue `outputs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferRequest {
    /// The notes being spent, each with its holder's signature.
    pub inputs: Vec<Spend>,
    /// The notes to create.
    pub outputs: Vec<Output>,
}

impl TransferRequest {
    /// Builds and signs a request. Each input note comes with the holder key
    /// it is bound to.
    ///
    /// Fails with [`Error::WrongKey`] if a key is not the one its note names.
    pub fn sign(inputs: &[(Note, &HolderKey)], outputs: Vec<Output>) -> Result<Self, Error> {
        for (input, (note, key)) in inputs.iter().enumerate() {
            if note.holder() != &key.public_key() {
                return Err(Error::WrongKey { input });
            }
        }
        let Some((first, _)) = inputs.first() else {
            return Ok(Self {
                inputs: Vec::new(),
                outputs,
            });
        };
        let serials: Vec<Serial> = inputs.iter().map(|(note, _)| note.serial()).collect();
        let message = message(first.issuer(), &serials, &outputs);
        let inputs = inputs
            .iter()
            .map(|(note, key)| Spend {
                note: *note,
                signature: key.sign(&message),
            })
            .collect();
        Ok(Self { inputs, outputs })
    }

    /// Signs a one-note transfer, the only kind [`Policy::cash_like`] allows
    /// besides splits.
    pub fn single(note: Note, key: &HolderKey, output: Output) -> Result<Self, Error> {
        Self::sign(&[(note, key)], vec![output])
    }

    /// The request's identity, as computed by the issuer that `issuer` names.
    pub fn id(&self, issuer: &PublicKey) -> TransferId {
        TransferId::of(&message(issuer, &self.serials(), &self.outputs))
    }

    fn serials(&self) -> Vec<Serial> {
        self.inputs
            .iter()
            .map(|spend| spend.note.serial())
            .collect()
    }
}

fn message(issuer: &PublicKey, serials: &[Serial], outputs: &[Output]) -> Vec<u8> {
    let mut w = Writer::with_capacity(DOMAIN.len() + 40 + serials.len() * 12 + outputs.len() * 40);
    w.bytes(DOMAIN).bytes(issuer.as_bytes());
    w.u32(serials.len() as u32);
    for serial in serials {
        w.u64(serial.generation).u32(serial.index);
    }
    w.u32(outputs.len() as u32);
    for output in outputs {
        w.u64(output.amount).bytes(output.holder.as_bytes());
    }
    w.finish()
}

/// SHA-256 of a transfer's signed message. Two requests with the same id
/// spend the same notes into the same outputs.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TransferId([u8; 32]);

impl TransferId {
    fn of(message: &[u8]) -> Self {
        Self(Sha256::digest(message).into())
    }

    /// Wraps 32 bytes.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The 32 bytes.
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0
    }
}

impl fmt::Display for TransferId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

impl fmt::Debug for TransferId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TransferId({self})")
    }
}

impl FromStr for TransferId {
    type Err = hex::FromHexError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut out = [0u8; 32];
        hex::decode_to_slice(s, &mut out)?;
        Ok(Self(out))
    }
}

impl Serialize for TransferId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        hex_serde::serialize(&self.0, s)
    }
}

impl<'de> Deserialize<'de> for TransferId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        hex_serde::deserialize(d).map(Self)
    }
}

/// Checks transfer requests against one issuer's key and policy.
///
/// Verification needs no ledger state, so a server can run it before taking
/// the issuer's lock and keep the signature checks off the critical section.
/// Get one from [`Issuer::verifier`](crate::Issuer::verifier).
#[derive(Debug, Clone)]
pub struct Verifier {
    issuer: PublicKey,
    policy: Policy,
}

impl Verifier {
    pub(crate) fn new(issuer: PublicKey, policy: Policy) -> Self {
        Self { issuer, policy }
    }

    /// The issuer this verifier checks for.
    pub fn issuer(&self) -> &PublicKey {
        &self.issuer
    }

    /// The policy it checks against.
    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    /// Checks the request's shape against the policy, every input note's
    /// issuer signature, that no note appears twice and every holder
    /// signature. Cheap checks run first.
    pub fn verify(&self, request: &TransferRequest) -> Result<VerifiedTransfer, Error> {
        let inputs: Vec<u64> = request.inputs.iter().map(|s| s.note.amount()).collect();
        let outputs: Vec<u64> = request.outputs.iter().map(|o| o.amount).collect();
        self.policy.check_transfer(&inputs, &outputs)?;

        let serials = request.serials();
        let mut sorted = serials.clone();
        sorted.sort_unstable();
        if let Some(pair) = sorted.windows(2).find(|pair| pair[0] == pair[1]) {
            return Err(Error::DuplicateInput(pair[0]));
        }
        for (input, spend) in request.inputs.iter().enumerate() {
            spend
                .note
                .verify(&self.issuer)
                .map_err(|error| Error::Note { input, error })?;
        }
        let message = message(&self.issuer, &serials, &request.outputs);
        for (input, spend) in request.inputs.iter().enumerate() {
            if !spend.note.holder().verify(&message, &spend.signature) {
                return Err(Error::Unauthorized { input });
            }
        }
        Ok(VerifiedTransfer {
            issuer: self.issuer,
            id: TransferId::of(&message),
            inputs: request.inputs.iter().map(|spend| spend.note).collect(),
            outputs: request.outputs.clone(),
        })
    }
}

/// A transfer request whose signatures and shape have been checked.
///
/// Only [`Verifier::verify`] creates one. Whether its inputs are still live
/// is decided later, against the ledger, by
/// [`Issuer::plan_transfer`](crate::Issuer::plan_transfer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedTransfer {
    pub(crate) issuer: PublicKey,
    pub(crate) id: TransferId,
    pub(crate) inputs: Vec<Note>,
    pub(crate) outputs: Vec<Output>,
}

impl VerifiedTransfer {
    /// The transfer's identity.
    pub fn id(&self) -> TransferId {
        self.id
    }

    /// The input notes.
    pub fn inputs(&self) -> &[Note] {
        &self.inputs
    }

    /// The requested outputs.
    pub fn outputs(&self) -> &[Output] {
        &self.outputs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::IssuerKey;
    use crate::note::NoteError;

    struct Fixture {
        issuer: IssuerKey,
        verifier: Verifier,
    }

    impl Fixture {
        fn new(policy: Policy) -> Self {
            let issuer = IssuerKey::generate();
            let verifier = Verifier::new(issuer.public_key(), policy);
            Self { issuer, verifier }
        }

        fn note(&self, index: u32, amount: u64) -> (Note, HolderKey) {
            let holder = HolderKey::generate();
            let note = Note::issue(
                &self.issuer,
                Serial::new(1, index),
                amount,
                holder.public_key(),
            );
            (note, holder)
        }
    }

    fn pay(amount: u64) -> Output {
        Output {
            amount,
            holder: HolderKey::generate().public_key(),
        }
    }

    #[test]
    fn a_signed_request_verifies() {
        let f = Fixture::new(Policy::cash_like());
        let (note, key) = f.note(0, 8);
        let request = TransferRequest::single(note, &key, pay(8)).unwrap();
        let verified = f.verifier.verify(&request).unwrap();
        assert_eq!(verified.inputs(), &[note]);
        assert_eq!(verified.id(), request.id(&f.issuer.public_key()));
    }

    #[test]
    fn signing_needs_the_notes_own_key() {
        let f = Fixture::new(Policy::cash_like());
        let (note, _) = f.note(0, 8);
        let stranger = HolderKey::generate();
        assert_eq!(
            TransferRequest::single(note, &stranger, pay(8)),
            Err(Error::WrongKey { input: 0 })
        );
    }

    #[test]
    fn redirecting_outputs_breaks_the_holder_signature() {
        let f = Fixture::new(Policy::cash_like());
        let (note, key) = f.note(0, 8);
        let mut request = TransferRequest::single(note, &key, pay(8)).unwrap();
        request.outputs[0].holder = HolderKey::generate().public_key();
        assert_eq!(
            f.verifier.verify(&request),
            Err(Error::Unauthorized { input: 0 })
        );
    }

    #[test]
    fn signatures_cannot_move_between_requests() {
        let f = Fixture::new(Policy::flexible());
        let (a, ka) = f.note(0, 5);
        let (b, kb) = f.note(1, 5);
        let both = TransferRequest::sign(&[(a, &ka), (b, &kb)], vec![pay(10)]).unwrap();
        // Reuse a's signature from the two-note request in a one-note request.
        let alone = TransferRequest {
            inputs: vec![both.inputs[0].clone()],
            outputs: vec![pay(5)],
        };
        assert_eq!(
            f.verifier.verify(&alone),
            Err(Error::Unauthorized { input: 0 })
        );
    }

    #[test]
    fn rejects_duplicates_foreign_notes_and_policy_breaches() {
        let f = Fixture::new(Policy::flexible());
        let (note, key) = f.note(0, 4);
        let twice = TransferRequest::sign(&[(note, &key), (note, &key)], vec![pay(8)]).unwrap();
        assert_eq!(
            f.verifier.verify(&twice),
            Err(Error::DuplicateInput(note.serial()))
        );

        let other = Fixture::new(Policy::flexible());
        let (foreign, fkey) = other.note(0, 4);
        let request = TransferRequest::single(foreign, &fkey, pay(4)).unwrap();
        assert_eq!(
            f.verifier.verify(&request),
            Err(Error::Note {
                input: 0,
                error: NoteError::WrongIssuer
            })
        );

        let cash = Fixture::new(Policy::cash_like());
        let (note, key) = cash.note(0, 4);
        let odd = TransferRequest::sign(&[(note, &key)], vec![pay(3), pay(1)]).unwrap();
        assert!(matches!(cash.verifier.verify(&odd), Err(Error::Policy(_))));
    }

    #[test]
    fn transfer_ids_round_trip_as_hex() {
        let id = TransferId::from_bytes([0xab; 32]);
        assert_eq!(id.to_string().parse::<TransferId>(), Ok(id));
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(serde_json::from_str::<TransferId>(&json).unwrap(), id);
    }
}
