use thiserror::Error;

use crate::ledger::{ConfigError, Serial};
use crate::note::NoteError;
use crate::policy::PolicyError;

/// Why the issuer refused a mint or transfer, or a holder could not sign one.
///
/// [`Policy`](Error::Policy), [`Note`](Error::Note),
/// [`DuplicateInput`](Error::DuplicateInput) and
/// [`WrongKey`](Error::WrongKey) mean the request itself is malformed;
/// [`Unauthorized`](Error::Unauthorized) means a signature is missing or
/// wrong; the rest are refusals by the ledger that depend on its state.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum Error {
    /// The request breaks the issuer's policy.
    #[error(transparent)]
    Policy(#[from] PolicyError),
    /// An input note is malformed, forged or from another issuer.
    #[error("input {input}: {error}")]
    Note {
        /// Index of the input.
        input: usize,
        /// What is wrong with it.
        error: NoteError,
    },
    /// The same note appears twice among the inputs.
    #[error("note {0} appears twice")]
    DuplicateInput(Serial),
    /// A holder key does not match the note it was meant to sign for.
    #[error("input {input}: key is not the note's holder key")]
    WrongKey {
        /// Index of the input.
        input: usize,
    },
    /// An input's holder signature does not verify.
    #[error("input {input}: holder signature does not verify")]
    Unauthorized {
        /// Index of the input.
        input: usize,
    },
    /// An input was already spent.
    #[error("note {0} is already spent")]
    Spent(Serial),
    /// An input's layer has been dropped.
    #[error("note {0} has expired")]
    Expired(Serial),
    /// The issuer signed this note but its ledger has no record of it, which
    /// only happens if the issuer's state was lost or rolled back.
    #[error("note {0} is unknown to the ledger")]
    Unissued(Serial),
    /// An input is in the last layer, which cannot transfer until the next rotation.
    #[error("note {0} is in the last layer and cannot move until the next rotation")]
    Frozen(Serial),
    /// The layer the outputs would land in has too few free bits left.
    #[error("layer {position} is full until the next rotation")]
    LayerFull {
        /// Position of the full layer.
        position: u32,
    },
}

/// The issuer could not be created or restored.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum SetupError {
    /// The ledger shape is outside its limits.
    #[error(transparent)]
    Ledger(#[from] ConfigError),
    /// The policy is incoherent or does not fit the ledger.
    #[error(transparent)]
    Policy(#[from] PolicyError),
    /// The snapshot is not one this crate wrote.
    #[error("snapshot is malformed: {0}")]
    Snapshot(#[from] crate::DecodeError),
    /// The snapshot's checksum does not match: it is corrupt.
    #[error("snapshot checksum does not match")]
    Checksum,
    /// The snapshot belongs to an issuer with a different key.
    #[error("snapshot belongs to a different issuer key")]
    KeyMismatch,
}

/// An event does not fit the issuer's current state, so it was not applied.
///
/// When replaying a journal this means the journal and snapshot disagree.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("event does not apply to the current state: {0}")]
pub struct ApplyError(pub(crate) String);
