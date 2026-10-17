//! Centralized, account-free e-cash with one bit of issuer state per note.
//!
//! An issuer signs banknotes with Ed25519 and tracks each one with a single
//! bit in a layered bit array: 1 while the note is live, 0 once it is spent.
//! A transfer clears the input notes' bits and issues new notes to the
//! receivers, so a note can never be spent twice. There are no accounts and
//! no wallets at the issuer: a note is bound to a fresh one-time key of
//! whoever receives it. The issuer stores no notes, only the bits, a few
//! counters and a bounded list of recent transfers that makes retries safe.
//!
//! The bit array is split into layers. A transferred note is reissued one
//! layer further on, and a note in the last layer cannot move. Every so
//! often the issuer drops the first layer and opens a new last layer, which
//! frees frozen notes to move again and destroys notes nobody touched for as
//! many rotations as there are layers. The rotation period therefore sets
//! both how fast money can circulate and how long it can sit idle.
//!
//! # Privacy
//!
//! The issuer learns no identities and keeps no accounts. It does see each
//! transfer's inputs die and its outputs appear, so it can follow the lineage
//! of every note. What it cannot do is tell who holds a note, or tell that
//! two unrelated lineages share an owner, as long as holders use a fresh key
//! for every note and never spend two notes in one transfer. The default
//! [`Policy::cash_like`] enforces the second rule; see [`policy`].
//!
//! This is weaker than blind-signature e-cash, where the issuer cannot link a
//! note to the one it replaced, and stronger than any account system. Blind
//! signatures are incompatible with the design: the issuer must choose each
//! note's serial, because the serial is the note's bit position.
//!
//! # Layout
//!
//! - [`ledger`]: the layered bit array, independent of notes and crypto.
//! - [`note`]: the signed note format.
//! - [`transfer`]: what holders sign to move notes, and its verification.
//! - [`policy`]: the rules an operator may change: denominations, inputs
//!   and outputs per transfer, where new money lands.
//! - [`issuer`]: ties the above together as an event-sourced state machine.
//! - [`store`]: crash-safe persistence as a snapshot plus a journal.
//!
//! # Example
//!
//! ```
//! use worgl::{HolderKey, Issuer, IssuerKey, LedgerConfig, NoteStatus, Output, Policy, TransferRequest};
//!
//! let ledger = LedgerConfig { layers: 4, layer_size: 1 << 16 };
//! let mut issuer = Issuer::new(IssuerKey::generate(), ledger, Policy::cash_like())?;
//!
//! // Alice makes a one-time key and is minted a note bound to it.
//! let alice = HolderKey::generate();
//! let note = issuer.mint(8, alice.public_key())?;
//!
//! // Bob makes a one-time key; Alice signs the note over to it.
//! let bob = HolderKey::generate();
//! let request = TransferRequest::single(note, &alice, Output { amount: 8, holder: bob.public_key() })?;
//! let bobs_note = issuer.transfer(&request)?.remove(0);
//!
//! assert_eq!(issuer.status(&note)?, NoteStatus::Spent);
//! assert!(matches!(issuer.status(&bobs_note)?, NoteStatus::Live { position: 3, .. }));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod codec;
mod error;
mod hex_serde;
pub mod issuer;
pub mod keys;
pub mod ledger;
pub mod note;
pub mod policy;
pub mod store;
pub mod transfer;

pub use codec::DecodeError;
pub use error::{ApplyError, Error, SetupError};
pub use issuer::{Event, Issuer, NoteStatus, Plan, RotationReport, Supply};
pub use keys::{HolderKey, IssuerKey, PublicKey, Signature};
pub use ledger::{Ledger, LedgerConfig, Serial};
pub use note::{Note, NoteError};
pub use policy::{Denominations, Policy, PolicyError};
pub use store::{CommitError, FileStore, PersistentIssuer, StoreError};
pub use transfer::{Output, Spend, TransferId, TransferRequest, VerifiedTransfer, Verifier};
