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
//!    transfer at its time, records each answer with [`Holder::settle`],
//!    which checks it against the mint's keys and takes the new notes in,
//!    and hands the rest of the answers, [`Holder::theirs`], back to the
//!    payer: its change.
//! 4. The payer checks their signatures and takes them in with
//!    [`Holder::take_answers`], which settles the transfers they answer.
//!    Without them, it resends its outgoing transfers at the times
//!    [`Holder::resend`] gives and records each answer with
//!    [`Holder::settle`], which works while the mint keeps its answers, and
//!    keeps the payee's notes from it in [`Holder::payments`], in case the
//!    payee has not submitted.
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
//! // Until the mint answers, 5 is on its way to Bob and 3 back to Alice.
//! let balance = alice.balance(&view(&mint)?);
//! assert_eq!((balance.sending, balance.change, balance.total()), (5, 3, 3));
//! for transfer in &payment.transfers {
//!     let answer = mint.transfer(&transfer.request)?;
//!     // Bob checks the answer against the mint's keys and keeps his notes.
//!     bob.settle(mint.keys(), &transfer.request, &answer)?;
//!     // Bob hands Alice the rest, her change; she checks it and keeps it.
//!     let change = bob.theirs(&answer);
//!     for note in &change {
//!         note.verify(mint.keys())?;
//!     }
//!     alice.take_answers(&change);
//! }
//!
//! // Bob's 5 is frozen until the next rotation; Alice has 3 in change.
//! assert_eq!(bob.balance(&view(&mint)?).frozen, 5);
//! assert_eq!(alice.balance(&view(&mint)?).frozen, 3);
//! assert_eq!(alice.outgoing().count(), 0);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! A mint's info, read into a [`MintView`], tells the store which notes can
//! move now: a note that just arrived sits in the last layer and is frozen
//! until the next rotation. The view holds the info's layers, which no clock
//! moves, so read the info again after a rotation. [`Holder::refresh`] keeps
//! notes alive by paying the ones about to expire to the holder itself.
//! With a schedule in the info, [`MintView::spendable_at`] and
//! [`MintView::expires_at`] say when a note can move and when it dies
//! untouched, and [`Holder::first_expiry`] when the first of the holder's
//! would: the time to refresh by.
//!
//! [`Holder::balance`] splits what a holder has at a mint into parts that do
//! not overlap: what it holds, what its transfers not yet answered pay
//! others and pay it back, and what it asked payers for. Its
//! [total](Balance::total) adds what it holds and what comes back to it as
//! change and refreshes, counting no note twice.

mod holder;
mod mint;
mod payment;
mod random;

pub use holder::{
    AnswerError, Balance, Holder, LoadError, MAX_REQUESTED_NOTES, MadePayment, PAID_KEYS_KEPT,
    PayError, REFRESH_MARGIN_SECS, RequestError,
};
pub use mint::{MintInfo, MintView, NoLayers, NoteState, Schedule};
pub use payment::{CheckError, Payment, PaymentRequest, ScheduledTransfer, Spacing};
