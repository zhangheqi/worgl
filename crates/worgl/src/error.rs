use thiserror::Error;

use crate::keys::{KeyError, PublicKey};
use crate::ledger::{ConfigError, Serial};
use crate::note::NoteError;
use crate::policy::PolicyError;

/// Why the issuer refused a mint or transfer, or a holder could not sign one.
///
/// [`Policy`](Error::Policy), [`Note`](Error::Note),
/// [`DuplicateInput`](Error::DuplicateInput) and
/// [`WrongKey`](Error::WrongKey) mean the request itself is malformed;
/// [`Unauthorized`](Error::Unauthorized),
/// [`UnknownAuthority`](Error::UnknownAuthority) and
/// [`MintUnauthorized`](Error::MintUnauthorized) mean a signature is
/// missing or wrong; [`Misfit`](Error::Misfit) means the issuer is at
/// fault; the rest are refusals that depend on the issuer's state.
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
    /// The inputs spend more from a layer than the issuer counts as live in
    /// it. This happens only after the issuer's state was rolled back and it
    /// handed out the same serials again, so that a note from before the
    /// rollback took its own amount from the layer's count rather than the
    /// amount issued on its serial.
    #[error("layer {position} holds less live value than the inputs spend from it")]
    Overdrawn {
        /// Position of the layer.
        position: u32,
    },
    /// An input is in the last layer, which cannot transfer until the next rotation.
    #[error("note {0} is in the last layer and cannot move until the next rotation")]
    Frozen(Serial),
    /// The layer the outputs would land in has too few free bits left.
    #[error("layer {position} has too few free bits left until a rotation drops it")]
    LayerFull {
        /// Position of the full layer.
        position: u32,
    },
    /// A mint request names a key the policy does not list as a mint authority.
    #[error("key {0} is not a mint authority of this issuer")]
    UnknownAuthority(PublicKey),
    /// A mint request's signature does not verify.
    #[error("the mint authority's signature does not verify")]
    MintUnauthorized,
    /// The issuer is paused: it mints, moves, burns and rotates nothing
    /// until it resumes.
    #[error("the issuer is paused")]
    Paused,
    /// The issuer holds as many receipts as its policy allows, and the
    /// oldest counts as younger than the policy's `receipt_min_age_secs`: a
    /// new one would push out the answer to a request its client may still
    /// resend. Nothing was done.
    #[error(
        "the issuer must keep the answers it holds for {retry_after_secs} s more before it takes \
         another transfer or burn"
    )]
    Busy {
        /// Seconds until the oldest receipt reaches the minimum age.
        retry_after_secs: u32,
    },
    /// A scheduled rotation is due no later than the last one logged, so
    /// that one already covered it.
    #[error("a rotation due at {due} is not after the last scheduled one, due at {last}")]
    StaleRotation {
        /// When the refused rotation was due, in seconds since the Unix epoch.
        due: u64,
        /// When the last scheduled rotation was due.
        last: u64,
    },
    /// A mint request reuses a sequence number, or goes below one, that its
    /// authority already used.
    #[error("mint sequence {sequence} is not above {last}, the authority's last")]
    StaleMint {
        /// The request's sequence number.
        sequence: u64,
        /// The last sequence number accepted from the authority.
        last: u64,
    },
    /// The event planned for the request does not apply to the issuer's
    /// state, so nothing was changed. Planning checks everything applying
    /// does, so this is a bug in the issuer, not a fault of the request.
    #[error(transparent)]
    Misfit(#[from] ApplyError),
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
    /// The snapshot or key ring belongs to an issuer with a different key.
    #[error("snapshot or key ring belongs to a different issuer key")]
    KeyMismatch,
    /// The key ring lacks the key of a live generation.
    #[error("the key ring lacks the key of generation {generation}")]
    MissingKey {
        /// The generation whose key is missing.
        generation: u64,
    },
    /// A generation key could not be made.
    #[error(transparent)]
    Key(#[from] KeyError),
}

/// An event does not fit the issuer's current state, so it was not applied.
///
/// When replaying a log this means the log and snapshot disagree.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("event does not apply to the current state: {0}")]
pub struct ApplyError(pub(crate) String);
