//! The holder's side of [Worgl](worgl): a note store that pays, receives and
//! refreshes notes while keeping the privacy rules.
//!
//! The mint sees every note's lineage but not who holds what. That stays
//! true only if holders follow a few rules, and this crate follows them so
//! applications need not know them:
//!
//! - **A fresh key for every note.** Two notes under one key tell the mint
//!   they share an owner.
//! - **One note per transfer.** A transfer that spends two notes tells the
//!   mint they had one owner. Change goes back to fresh keys.
//! - **Spread in time.** When a payment needs several notes, their
//!   transfers are submitted at random gaps; refreshes are spread across the
//!   rotation period. Transfers made together can be linked by timing alone.
//! - **No order to read.** Outputs are shuffled, so the payee's notes are
//!   not always first.
//!
//! The crate does no networking and keeps no files, so it runs anywhere,
//! WebAssembly included. A client submits the transfers it makes and hands
//! back the mint's answers.
//!
//! # A payment
//!
//! 1. The payee calls [`Holder::request`] and sends the [`PaymentRequest`]
//!    to the payer: an amount as notes, each to a fresh key of the payee.
//! 2. The payer calls [`Holder::pay`] and returns the [`Payment`]: signed
//!    transfers, each spending one of its notes, with a time to submit each.
//! 3. The payee checks it with [`PaymentRequest::check`], submits each
//!    transfer at its time, and takes the new notes in with
//!    [`Holder::absorb`].
//! 4. The payer gets the same answers, by resending its
//!    [outgoing](Holder::outgoing) transfers or from the payee, and records
//!    them with [`Holder::settle`], which takes in its change.
//!
//! ```
//! use worgl::{Issuer, IssuerKey, LedgerConfig, Policy};
//! use worgl_sdk::{Holder, MintInfo, MintView, Spacing};
//!
//! // A mint in memory, standing in for the server.
//! let ledger = LedgerConfig { layers: 4, layer_size: 1 << 16 };
//! let mut mint = Issuer::new(IssuerKey::generate(), ledger, Policy::cash_like())?;
//! let view = |mint: &Issuer| {
//!     MintView::new(&MintInfo {
//!         issuer: *mint.public_key(),
//!         layers: mint.layers(),
//!         policy: mint.policy().clone(),
//!         rotation: None,
//!         paused: false,
//!     })
//! };
//!
//! // Alice is minted 8 in one note.
//! let (mut alice, mut bob) = (Holder::new(), Holder::new());
//! let funding = alice.request(&view(&mint)?, 8)?;
//! let note = mint.mint(8, funding.outputs[0].holder)?;
//! alice.absorb(&[note]);
//!
//! // Bob asks for 5, Alice pays, Bob checks and submits.
//! let request = bob.request(&view(&mint)?, 5)?;
//! let payment = alice.pay(&view(&mint)?, &request, Spacing::default())?;
//! request.check(&payment)?;
//! for transfer in &payment.transfers {
//!     let answer = mint.transfer(&transfer.request)?;
//!     bob.absorb(&answer);
//!     alice.settle(&transfer.request, &answer);
//! }
//!
//! // Bob's 5 is frozen until the next rotation; Alice has 3 in change.
//! assert_eq!(bob.balance(&view(&mint)?).frozen, 5);
//! assert_eq!(alice.balance(&view(&mint)?).frozen, 3);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! A mint's info, read into a [`MintView`], tells the store which notes can
//! move now: a note that just arrived sits in the last layer and is frozen
//! until the next rotation. [`Holder::refresh`] keeps notes alive by paying
//! the ones about to expire to the holder itself.

mod holder;
mod mint;
mod payment;
mod random;

pub use holder::{Balance, Holder, LoadError, MAX_REQUESTED_NOTES, PayError, RequestError};
pub use mint::{MintInfo, MintView, NoLayers, NoteState, Schedule};
pub use payment::{CheckError, Payment, PaymentRequest, ScheduledTransfer, Spacing};
