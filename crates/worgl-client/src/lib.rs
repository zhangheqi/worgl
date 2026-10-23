//! Talks to a [Worgl](worgl) server over HTTP.
//!
//! Two layers:
//!
//! - [`Client`] makes one typed call per route of `worgl-server`: mint info
//!   and keys, verify, transfer, burn, mint, and the mint admin's rotate,
//!   pause and resume, and the two routes of a playground faucet. Errors
//!   keep the mint's stable code, such as `spent` or `frozen`, in
//!   [`Error::code`].
//! - [`Purse`] puts a [`worgl_sdk::Holder`] on top: it reads mint info by
//!   itself, submits transfers at the random times the holder gives them,
//!   retries what failed on the way, checks each answer against the mint's
//!   keys, and saves the holder to a [`Store`] after every change, before
//!   anything that depends on it is handed out.
//!
//! The privacy rules come from the holder: a fresh key for every note, one
//! note per transfer, and transfers of one payment, refreshes and resends
//! spread out in time.
//!
//! # A payment
//!
//! ```no_run
//! # async fn run(mint: worgl::PublicKey) -> Result<(), worgl_client::Error> {
//! use worgl_client::{Client, Purse};
//! use worgl_sdk::Spacing;
//!
//! let client = Client::new("https://mint.example.com")?;
//! let alice = Purse::open(client.clone(), "alice.json")?;
//! let bob = Purse::open(client, "bob.json")?;
//!
//! // Bob asks for 5 and sends the request to Alice.
//! let request = bob.request(&mint, 5).await?;
//! // Alice answers with signed transfers and sends them to Bob.
//! let payment = alice.pay(&request, Spacing::default()).await?;
//! // Bob checks them, submits each at its time and keeps the notes.
//! let received = bob.receive(&request, &payment).await?;
//! assert_eq!(received.value, 5);
//! // Bob sends Alice the rest of the answers, her change, and she keeps it.
//! alice.accept(&received.theirs).await?;
//! # Ok(())
//! # }
//! ```
//!
//! Without the change from Bob, Alice would [`settle`](Purse::settle):
//! resend her transfers, which the mint answers with the same notes while
//! it keeps its answers. If Bob had not submitted the payment, her resend
//! carries it out. She cannot tell, so she keeps his notes from every
//! answer for him (see [`Holder::payments`](worgl_sdk::Holder::payments))
//! until she hands them over ([`Purse::forget_owed`]) or
//! [prunes](Purse::prune) them once they expire. A transfer the mint
//! refuses for good is dropped or cancelled ([`Refused::outcome`]).
//!
//! Either side may submit a payment. When the payer does, with
//! [`Purse::submit`], it hands the payee's notes from the answers,
//! [`Submitted::theirs`], to the payee, who takes them in with
//! [`Purse::accept`], and then stops keeping them with
//! [`Purse::forget_owed`].
//!
//! Requests run on Tokio. TLS is rustls with the platform's certificate
//! roots.

mod client;
mod error;
mod purse;
mod store;

pub use client::{Client, FaucetInfo, Info, Verified};
pub use error::{Error, Refusal};
pub use purse::{MAX_DELAY_SECS, Outcome, Purse, Refused, Retry, Submitted};
pub use store::{FileLock, FileStore, Locked, MemoryStore, Store};
