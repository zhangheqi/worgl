//! What can go wrong talking to a mint or keeping a purse.

use std::fmt;
use std::io;
use std::time::Duration;

use serde::Deserialize;
use worgl::NoteError;
use worgl_sdk::{CheckError, LoadError, NoLayers, PayError, RequestError};

/// An error from a [`Client`](crate::Client) or a [`Purse`](crate::Purse).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The request did not complete: no connection, a timeout, or an answer
    /// that was not what the route returns. A transfer may still have been
    /// applied; resending it is safe.
    #[error("talking to the mint failed: {0}")]
    Http(#[from] reqwest::Error),
    /// The mint answered with an error.
    #[error("the mint refused: {0}")]
    Refused(Refusal),
    /// The server URL is not an `http` or `https` URL.
    #[error("{0} is not an http or https URL")]
    Url(String),
    /// The keys a mint served belong to another issuer.
    #[error("the mint served the signing keys of another issuer")]
    Keys,
    /// A note handed over does not carry the mint's signature.
    #[error("a note is not signed by its mint: {0}")]
    Note(#[from] NoteError),
    /// The mint's info lists no layers.
    #[error(transparent)]
    NoLayers(#[from] NoLayers),
    /// A payment request could not be made.
    #[error(transparent)]
    Request(#[from] RequestError),
    /// A payment could not be made.
    #[error(transparent)]
    Pay(#[from] PayError),
    /// A playground faucet hands out notes of an amount its mint cannot
    /// make as one note.
    #[error("the faucet hands out notes of {0}, which its mint cannot make as one note")]
    FaucetAmount(u64),
    /// A payment does not pay what was asked.
    #[error(transparent)]
    Check(#[from] CheckError),
    /// The store could not be read.
    #[error("the saved store cannot be read: {0}")]
    Load(#[from] LoadError),
    /// The store could not be read or written. Nothing was handed out that
    /// depends on the change that failed to save.
    #[error("the store could not be saved or read: {0}")]
    Store(#[from] io::Error),
}

impl Error {
    /// The mint's error code, when the mint answered with one; see
    /// [`Refusal::code`].
    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Refused(refusal) => Some(&refusal.code),
            _ => None,
        }
    }

    /// Whether trying again later may succeed: the request did not complete,
    /// or the mint was paused, busy or failed to store the change. Transfers
    /// and burns are safe to resend.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Http(error) => !error.is_decode() && !error.is_builder(),
            Self::Refused(refusal) => {
                matches!(
                    refusal.code.as_str(),
                    "paused" | "busy" | "storage" | "internal" | "layer_full"
                ) || refusal.status >= 500
            }
            _ => false,
        }
    }

    /// How long the mint asked to wait before trying again, when it
    /// answered with a `Retry-After`, as it does with `busy`; see
    /// [`Refusal::retry_after`].
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Refused(refusal) => refusal.retry_after,
            _ => None,
        }
    }
}

/// An error answer from the mint.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Refusal {
    /// The HTTP status.
    #[serde(skip)]
    pub status: u16,
    /// A stable code: `spent`, `expired`, `frozen`, `paused`, `policy`,
    /// `unknown_mint` and so on, as the server documents them. `http` when
    /// the answer carried none, such as from a proxy.
    #[serde(rename = "error")]
    pub code: String,
    /// What went wrong, for people.
    pub message: String,
    /// How long to wait before sending the request again, from the
    /// answer's `Retry-After` in seconds. A `busy` mint sends one; it took
    /// nothing, and has room again after that.
    #[serde(skip)]
    pub retry_after: Option<Duration>,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({}, HTTP {})", self.message, self.code, self.status)
    }
}
