//! What a payee asks for and what a payer hands over.

use serde::{Deserialize, Serialize};
use worgl::{Output, PublicKey, TransferRequest};

/// What a payee asks to be paid: one fresh key of its own for each note it
/// expects, so the mint never sees two of its notes under one key.
///
/// Make one with [`Holder::request`](crate::Holder::request) and hand it to
/// the payer, who answers with a [`Payment`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentRequest {
    /// The mint whose notes the payee accepts.
    pub mint: PublicKey,
    /// The notes to be created for the payee: an amount and a fresh key
    /// each.
    pub outputs: Vec<Output>,
}

impl PaymentRequest {
    /// The total asked for.
    pub fn amount(&self) -> u128 {
        self.outputs.iter().map(|o| u128::from(o.amount)).sum()
    }

    /// Checks that a payment pays exactly what this request asks: every
    /// transfer spends one note of the requested mint, and every requested
    /// output appears in exactly one transfer.
    ///
    /// It cannot tell whether the payer's notes are still live; submitting
    /// the transfers does.
    pub fn check(&self, payment: &Payment) -> Result<(), CheckError> {
        if payment.mint != self.mint {
            return Err(CheckError::WrongMint);
        }
        let mut wanted = self.outputs.clone();
        for scheduled in &payment.transfers {
            let request = &scheduled.request;
            let [spend] = request.inputs.as_slice() else {
                return Err(CheckError::NotOneInput);
            };
            if spend.note.issuer() != &self.mint {
                return Err(CheckError::WrongMint);
            }
            for output in &request.outputs {
                if let Some(i) = wanted.iter().position(|w| w == output) {
                    wanted.swap_remove(i);
                } else if self.outputs.iter().any(|w| w.holder == output.holder) {
                    return Err(CheckError::Duplicate(*output));
                }
            }
        }
        match wanted.first() {
            Some(missing) => Err(CheckError::Missing(*missing)),
            None => Ok(()),
        }
    }
}

/// A payment does not match its request.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CheckError {
    /// It spends notes of another mint.
    #[error("the payment spends notes of another mint")]
    WrongMint,
    /// A transfer does not spend exactly one note.
    #[error("a transfer spends more or fewer than one note")]
    NotOneInput,
    /// A requested note is not paid.
    #[error("the payment does not pay {} to {}", .0.amount, .0.holder)]
    Missing(Output),
    /// A requested key is paid more than once, or with the wrong amount.
    #[error("the payment pays {} to {} more than asked", .0.amount, .0.holder)]
    Duplicate(Output),
}

/// The payer's answer to a [`PaymentRequest`]: signed transfers that,
/// submitted to the mint, create the payee's notes.
///
/// Either side may submit them. Each spends one of the payer's notes and
/// pays any change to fresh keys of the payer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Payment {
    /// The mint the transfers go to.
    pub mint: PublicKey,
    /// The transfers, each with when to submit it.
    pub transfers: Vec<ScheduledTransfer>,
}

/// A signed transfer and when to submit it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledTransfer {
    /// Seconds to wait before submitting, counted from when the transfers
    /// were made. Transfers sent together tell the mint their notes had one
    /// owner, so the SDK spreads them out.
    pub after_secs: u64,
    /// The transfer.
    pub request: TransferRequest,
}

/// How far apart to submit the transfers of one payment when it takes more
/// than one, in seconds. Each gap is drawn at random from this range.
///
/// Closer spacing pays faster but lets the mint guess, from timing alone,
/// that the notes had one owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spacing {
    /// Shortest gap.
    pub min_secs: u64,
    /// Longest gap.
    pub max_secs: u64,
}

impl Spacing {
    /// One gap, drawn at random.
    pub(crate) fn gap(self) -> u64 {
        crate::random::between(self.min_secs, self.max_secs)
    }
}

impl Default for Spacing {
    /// Gaps of 2 to 60 seconds.
    fn default() -> Self {
        Self {
            min_secs: 2,
            max_secs: 60,
        }
    }
}
