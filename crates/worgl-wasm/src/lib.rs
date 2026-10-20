//! WebAssembly bindings of [`worgl_sdk`], the core of the TypeScript SDK in
//! `sdk/typescript`.
//!
//! Protocol objects (payment requests, payments, transfers, notes, a mint's
//! info and keys) cross the boundary as JSON text, exactly as the server and
//! the Rust crates write them, so amounts beyond what a JavaScript number
//! holds survive the trip. Amounts JavaScript reads are `bigint`.
//!
//! Every privacy rule lives in the Rust core; the TypeScript side only moves
//! text between the holder, the server and its store.

use serde::Serialize;
use serde::de::DeserializeOwned;
use wasm_bindgen::prelude::*;
use worgl::issuer::LayerSummary;
use worgl::{KeySet, Note, NoteStatus, PublicKey, Serial, Supply, TransferRequest};
use worgl_sdk::{
    MintInfo, MintView, Payment, PaymentRequest, Schedule, ScheduledTransfer, Spacing,
};

/// The notes one holder owns, with their keys; see [`worgl_sdk::Holder`].
///
/// Methods that need to know a mint's state take its `info` as the server
/// returned it and the current Unix time in seconds.
#[wasm_bindgen]
#[derive(Debug, Default)]
pub struct Holder {
    inner: worgl_sdk::Holder,
}

#[wasm_bindgen]
impl Holder {
    /// An empty holder.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads a holder saved with [`to_json`](Self::to_json).
    #[wasm_bindgen(js_name = fromJson)]
    pub fn from_json(json: &str) -> Result<Holder, JsError> {
        let inner = worgl_sdk::Holder::from_json(json).map_err(error)?;
        Ok(Self { inner })
    }

    /// The whole holder as JSON, secret keys included.
    #[wasm_bindgen(js_name = toJson)]
    pub fn to_json(&self) -> String {
        self.inner.to_json()
    }

    /// A payment request for `amount`, one fresh key per note.
    pub fn request(&mut self, info: &str, now: u64, amount: u64) -> Result<String, JsError> {
        let view = view(info, now)?;
        Ok(json(&self.inner.request(&view, amount).map_err(error)?))
    }

    /// A payment request for `amount` in notes of at most `largest`.
    #[wasm_bindgen(js_name = requestAtMost)]
    pub fn request_at_most(
        &mut self,
        info: &str,
        now: u64,
        amount: u64,
        largest: u64,
    ) -> Result<String, JsError> {
        let view = view(info, now)?;
        let request = self
            .inner
            .request_at_most(&view, amount, largest)
            .map_err(error)?;
        Ok(json(&request))
    }

    /// Stops expecting the notes of a request.
    pub fn withdraw(&mut self, request: &str) -> Result<(), JsError> {
        self.inner.withdraw(&parse::<PaymentRequest>(request)?);
        Ok(())
    }

    /// Answers a payment request with signed transfers, spaced by gaps of
    /// `min_secs` to `max_secs`.
    pub fn pay(
        &mut self,
        info: &str,
        now: u64,
        request: &str,
        min_secs: u64,
        max_secs: u64,
    ) -> Result<String, JsError> {
        let view = view(info, now)?;
        let request = parse::<PaymentRequest>(request)?;
        let spacing = Spacing { min_secs, max_secs };
        let payment = self.inner.pay(&view, &request, spacing).map_err(error)?;
        Ok(json(&payment))
    }

    /// Takes in the notes, a JSON array, that pay keys this holder handed
    /// out. Returns the value taken in. Check signatures first with
    /// [`verify_notes`] unless the notes come straight from the mint.
    pub fn absorb(&mut self, notes: &str) -> Result<u128, JsError> {
        Ok(self.inner.absorb(&parse::<Vec<Note>>(notes)?))
    }

    /// The notes, a JSON array, made for neither a note this holder holds
    /// nor a key it expects: the counterparty's, to hand over. Returned as a
    /// JSON array.
    pub fn theirs(&self, notes: &str) -> Result<String, JsError> {
        Ok(json(&self.inner.theirs(&parse::<Vec<Note>>(notes)?)))
    }

    /// Records the mint's answer, a JSON array of notes, to a transfer.
    /// Returns the value taken in.
    pub fn settle(&mut self, transfer: &str, notes: &str) -> Result<u128, JsError> {
        let transfer = parse::<TransferRequest>(transfer)?;
        Ok(self.inner.settle(&transfer, &parse::<Vec<Note>>(notes)?))
    }

    /// Takes in notes, a JSON array, handed over from the mint's answers,
    /// and settles the outgoing transfers they answer. Returns the value
    /// taken in. Check signatures first with [`verify_notes`].
    #[wasm_bindgen(js_name = takeAnswers)]
    pub fn take_answers(&mut self, notes: &str) -> Result<u128, JsError> {
        Ok(self.inner.take_answers(&parse::<Vec<Note>>(notes)?))
    }

    /// What this holder may lose on a transfer if the mint applied it and
    /// its answer is lost: the value it pays to keys still expected, or
    /// `undefined` when the transfer is neither outgoing nor pays such a
    /// key.
    #[wasm_bindgen(js_name = atStake, unchecked_return_type = "bigint | undefined")]
    pub fn at_stake(&self, transfer: &str) -> Result<JsValue, JsError> {
        // An optional u128 does not fit a fallible return, so it goes as a value.
        let stake = self.inner.at_stake(&parse::<TransferRequest>(transfer)?);
        Ok(stake.map_or(JsValue::UNDEFINED, JsValue::from))
    }

    /// Drops an outgoing transfer whose note is gone, keeping the keys of
    /// its change. Returns whether it was outgoing.
    pub fn forget(&mut self, transfer: &str) -> Result<bool, JsError> {
        Ok(self.inner.forget(&parse::<TransferRequest>(transfer)?))
    }

    /// Takes back the note of an outgoing transfer the mint never applied,
    /// keeping the keys of its change. Returns whether it was outgoing.
    pub fn cancel(&mut self, transfer: &str) -> Result<bool, JsError> {
        Ok(self.inner.cancel(&parse::<TransferRequest>(transfer)?))
    }

    /// The transfers not yet answered, each as JSON.
    pub fn outgoing(&self) -> Vec<String> {
        self.inner.outgoing().map(json).collect()
    }

    /// The payments this holder made and keeps: while one of their transfers
    /// is outgoing, or it keeps notes owed to the payee.
    pub fn payments(&self) -> Vec<MadePayment> {
        self.inner
            .payments()
            .map(|made| MadePayment {
                request: json(&made.request),
                payment: json(&made.payment),
                owed: json(&made.owed),
            })
            .collect()
    }

    /// Stops keeping the notes, a JSON array, owed to payees, once they are
    /// handed over. Returns the value it stopped keeping.
    #[wasm_bindgen(js_name = forgetOwed)]
    pub fn forget_owed(&mut self, notes: &str) -> Result<u128, JsError> {
        Ok(self.inner.forget_owed(&parse::<Vec<Note>>(notes)?))
    }

    /// The outgoing transfers in random order, spaced by gaps of `min_secs`
    /// to `max_secs`, to resend.
    pub fn resend(&self, min_secs: u64, max_secs: u64) -> Vec<Scheduled> {
        let spacing = Spacing { min_secs, max_secs };
        scheduled(self.inner.resend(spacing))
    }

    /// The outgoing transfers to resend before the mint's next rotation,
    /// each at a random moment before it, as `refresh` spreads its own.
    #[wasm_bindgen(js_name = dueResends)]
    pub fn due_resends(&self, info: &str, now: u64) -> Result<Vec<Scheduled>, JsError> {
        let view = view(info, now)?;
        Ok(scheduled(self.inner.due_resends(&view, now)))
    }

    /// Pays the mint's notes that die at the next rotation back to fresh
    /// keys, each at a random moment before it.
    pub fn refresh(&mut self, info: &str, now: u64) -> Result<Vec<Scheduled>, JsError> {
        let view = view(info, now)?;
        Ok(scheduled(self.inner.refresh(&view, now)))
    }

    /// Drops the mint's expired notes and returns the value lost.
    pub fn prune(&mut self, info: &str, now: u64) -> Result<u128, JsError> {
        Ok(self.inner.prune(&view(info, now)?))
    }

    /// Value held at the mint, by what it can do now.
    pub fn balance(&self, info: &str, now: u64) -> Result<Balance, JsError> {
        let balance = self.inner.balance(&view(info, now)?);
        Ok(Balance {
            spendable: balance.spendable,
            frozen: balance.frozen,
            expiring: balance.expiring,
            unknown: balance.unknown,
            outgoing: balance.outgoing,
            expected: balance.expected,
            unconfirmed: balance.unconfirmed,
        })
    }

    /// The notes held, of every mint, as a JSON array.
    pub fn notes(&self) -> String {
        json(&self.inner.notes().collect::<Vec<_>>())
    }
}

/// A signed transfer and when to submit it.
#[wasm_bindgen]
#[derive(Debug)]
pub struct Scheduled {
    after_secs: u64,
    request: String,
}

#[wasm_bindgen]
impl Scheduled {
    /// Seconds to wait before submitting it.
    #[wasm_bindgen(getter, js_name = afterSecs)]
    pub fn after_secs(&self) -> u64 {
        self.after_secs
    }

    /// The transfer, as JSON, ready to post to `/v1/transfer`.
    #[wasm_bindgen(getter)]
    pub fn request(&self) -> String {
        self.request.clone()
    }
}

/// A payment a holder made and keeps; see [`worgl_sdk::MadePayment`].
#[wasm_bindgen]
#[derive(Debug)]
pub struct MadePayment {
    request: String,
    payment: String,
    owed: String,
}

#[wasm_bindgen]
impl MadePayment {
    /// The request it answers, as JSON.
    #[wasm_bindgen(getter)]
    pub fn request(&self) -> String {
        self.request.clone()
    }

    /// The payment, as JSON, to send again to a payee who lost it, less any
    /// transfer cancelled or forgotten.
    #[wasm_bindgen(getter)]
    pub fn payment(&self) -> String {
        self.payment.clone()
    }

    /// The notes kept for the payee, a JSON array.
    #[wasm_bindgen(getter)]
    pub fn owed(&self) -> String {
        self.owed.clone()
    }
}

fn scheduled(transfers: Vec<ScheduledTransfer>) -> Vec<Scheduled> {
    transfers
        .into_iter()
        .map(|t| Scheduled {
            after_secs: t.after_secs,
            request: json(&t.request),
        })
        .collect()
}

/// Value held at one mint; see [`worgl_sdk::Balance`].
#[wasm_bindgen]
#[derive(Debug, Clone, Copy)]
pub struct Balance {
    /// Value that can be spent now.
    #[wasm_bindgen(readonly)]
    pub spendable: u128,
    /// Value frozen in the last layer until the next rotation.
    #[wasm_bindgen(readonly)]
    pub frozen: u128,
    /// Value that dies at the next rotation unless refreshed.
    #[wasm_bindgen(readonly)]
    pub expiring: u128,
    /// Value in notes newer than the mint's info.
    #[wasm_bindgen(readonly)]
    pub unknown: u128,
    /// Value in transfers sent and not yet seen answered.
    #[wasm_bindgen(readonly)]
    pub outgoing: u128,
    /// Value asked for and not yet received.
    #[wasm_bindgen(readonly)]
    pub expected: u128,
    /// Change of transfers dropped or cancelled, which comes only if the
    /// counterparty hands it over.
    #[wasm_bindgen(readonly)]
    pub unconfirmed: u128,
}

/// One note a payment request asks for.
#[wasm_bindgen]
#[derive(Debug)]
pub struct RequestedNote {
    amount: u64,
    holder: String,
}

#[wasm_bindgen]
impl RequestedNote {
    /// Its value.
    #[wasm_bindgen(getter)]
    pub fn amount(&self) -> u64 {
        self.amount
    }

    /// The payee's one-time key for it, in hex.
    #[wasm_bindgen(getter)]
    pub fn holder(&self) -> String {
        self.holder.clone()
    }
}

/// The issuer key of the mint a payment request is for.
#[wasm_bindgen(js_name = requestMint)]
pub fn request_mint(request: &str) -> Result<String, JsError> {
    Ok(parse::<PaymentRequest>(request)?.mint.to_string())
}

/// The total a payment request asks for.
#[wasm_bindgen(js_name = requestAmount)]
pub fn request_amount(request: &str) -> Result<u128, JsError> {
    Ok(parse::<PaymentRequest>(request)?.amount())
}

/// The notes a payment request asks for.
#[wasm_bindgen(js_name = requestNotes)]
pub fn request_notes(request: &str) -> Result<Vec<RequestedNote>, JsError> {
    Ok(parse::<PaymentRequest>(request)?
        .outputs
        .into_iter()
        .map(|o| RequestedNote {
            amount: o.amount,
            holder: o.holder.to_string(),
        })
        .collect())
}

/// Checks that a payment pays exactly what the request asks.
#[wasm_bindgen(js_name = checkPayment)]
pub fn check_payment(request: &str, payment: &str) -> Result<(), JsError> {
    let request = parse::<PaymentRequest>(request)?;
    request.check(&parse::<Payment>(payment)?).map_err(error)
}

/// The issuer key of the mint a payment is for.
#[wasm_bindgen(js_name = paymentMint)]
pub fn payment_mint(payment: &str) -> Result<String, JsError> {
    Ok(parse::<Payment>(payment)?.mint.to_string())
}

/// A payment's transfers, each with when to submit it.
#[wasm_bindgen(js_name = paymentTransfers)]
pub fn payment_transfers(payment: &str) -> Result<Vec<Scheduled>, JsError> {
    Ok(scheduled(parse::<Payment>(payment)?.transfers))
}

/// The issuer key of the mint a transfer goes to: its first input's.
#[wasm_bindgen(js_name = transferMint)]
pub fn transfer_mint(transfer: &str) -> Result<String, JsError> {
    let transfer = parse::<TransferRequest>(transfer)?;
    let spend = transfer
        .inputs
        .first()
        .ok_or_else(|| JsError::new("the transfer spends no notes"))?;
    Ok(spend.note.issuer().to_string())
}

/// Whether a transfer breaks the policy in a mint's info.
#[wasm_bindgen(js_name = breaksPolicy)]
pub fn breaks_policy(info: &str, transfer: &str) -> Result<bool, JsError> {
    let policy = parse::<MintInfo>(info)?.policy;
    let transfer = parse::<TransferRequest>(transfer)?;
    let inputs: Vec<u64> = transfer.inputs.iter().map(|s| s.note.amount()).collect();
    let outputs: Vec<u64> = transfer.outputs.iter().map(|o| o.amount).collect();
    Ok(policy.check_transfer(&inputs, &outputs).is_err())
}

/// The issuer key of a note, given as a JSON string.
#[wasm_bindgen(js_name = noteIssuer)]
pub fn note_issuer(note: &str) -> Result<String, JsError> {
    Ok(parse::<Note>(note)?.issuer().to_string())
}

/// The issuer keys of the notes, a JSON array, each named once.
#[wasm_bindgen(js_name = noteIssuers)]
pub fn note_issuers(notes: &str) -> Result<Vec<String>, JsError> {
    let mut issuers: Vec<PublicKey> = parse::<Vec<Note>>(notes)?
        .iter()
        .map(|note| *note.issuer())
        .collect();
    issuers.sort_unstable();
    issuers.dedup();
    Ok(issuers.iter().map(PublicKey::to_string).collect())
}

/// Checks that each of the notes, a JSON array, is signed by the mint whose
/// key set `keys` is, as `GET /v1/mints/{mint}/keys` returns it. Throws on
/// the first that is not.
#[wasm_bindgen(js_name = verifyNotes)]
pub fn verify_notes(keys: &str, notes: &str) -> Result<(), JsError> {
    let keys = parse::<KeySet>(keys)?;
    for note in parse::<Vec<Note>>(notes)? {
        note.verify(&keys).map_err(error)?;
    }
    Ok(())
}

/// What `GET /v1/mints/{mint}/info` says, in numbers JavaScript can read.
#[wasm_bindgen]
#[derive(Debug)]
pub struct MintSummary {
    issuer: String,
    /// Events applied since the mint was created.
    #[wasm_bindgen(readonly)]
    pub sequence: u64,
    /// Number of layers.
    #[wasm_bindgen(readonly)]
    pub layers: u32,
    /// Generation of the first layer: rotations so far.
    #[wasm_bindgen(readonly)]
    pub oldest: u64,
    /// Whether the mint is paused.
    #[wasm_bindgen(readonly)]
    pub paused: bool,
    period_secs: Option<u64>,
    next_at: Option<u64>,
    /// Value ever minted.
    #[wasm_bindgen(readonly)]
    pub minted: u128,
    /// Value destroyed by rotations.
    #[wasm_bindgen(readonly)]
    pub destroyed: u128,
    /// Value burned by holders.
    #[wasm_bindgen(readonly)]
    pub burned: u128,
    /// Value in live notes.
    #[wasm_bindgen(readonly)]
    pub outstanding: u128,
}

#[wasm_bindgen]
impl MintSummary {
    /// The mint's issuer key, in hex.
    #[wasm_bindgen(getter)]
    pub fn issuer(&self) -> String {
        self.issuer.clone()
    }

    /// Seconds between scheduled rotations, if the mint has a schedule.
    #[wasm_bindgen(getter, js_name = periodSecs)]
    pub fn period_secs(&self) -> Option<u64> {
        self.period_secs
    }

    /// Unix time of the next scheduled rotation, if any.
    #[wasm_bindgen(getter, js_name = nextAt)]
    pub fn next_at(&self) -> Option<u64> {
        self.next_at
    }
}

#[derive(serde::Deserialize)]
struct InfoText {
    issuer: PublicKey,
    sequence: u64,
    layers: Vec<LayerSummary>,
    supply: Supply,
    #[serde(default)]
    rotation: Option<Schedule>,
    #[serde(default)]
    paused: bool,
}

/// Reads a mint's info.
#[wasm_bindgen(js_name = mintSummary)]
pub fn mint_summary(info: &str) -> Result<MintSummary, JsError> {
    let info = parse::<InfoText>(info)?;
    Ok(MintSummary {
        issuer: info.issuer.to_string(),
        sequence: info.sequence,
        layers: u32::try_from(info.layers.len()).unwrap_or(u32::MAX),
        oldest: info.layers.first().map_or(0, |layer| layer.generation),
        paused: info.paused,
        period_secs: info.rotation.map(|r| r.period_secs),
        next_at: info.rotation.map(|r| r.next_at),
        minted: info.supply.minted,
        destroyed: info.supply.destroyed,
        burned: info.supply.burned,
        outstanding: info.supply.outstanding,
    })
}

/// What `POST /v1/verify` says about a note.
#[wasm_bindgen]
#[derive(Debug)]
pub struct NoteReport {
    /// The note's generation.
    #[wasm_bindgen(readonly)]
    pub generation: u64,
    /// Its index in the generation.
    #[wasm_bindgen(readonly)]
    pub index: u32,
    /// Its amount.
    #[wasm_bindgen(readonly)]
    pub amount: u64,
    state: &'static str,
    position: Option<u32>,
    hops_left: Option<u32>,
    expires_after_rotations: Option<u32>,
}

#[wasm_bindgen]
impl NoteReport {
    /// `live`, `spent`, `expired` or `unissued`.
    #[wasm_bindgen(getter)]
    pub fn state(&self) -> String {
        self.state.into()
    }

    /// For a live note, the position of its layer.
    #[wasm_bindgen(getter)]
    pub fn position(&self) -> Option<u32> {
        self.position
    }

    /// For a live note, the transfers its lineage can still make before the
    /// next rotation.
    #[wasm_bindgen(getter, js_name = hopsLeft)]
    pub fn hops_left(&self) -> Option<u32> {
        self.hops_left
    }

    /// For a live note, the rotations until it expires untouched.
    #[wasm_bindgen(getter, js_name = expiresAfterRotations)]
    pub fn expires_after_rotations(&self) -> Option<u32> {
        self.expires_after_rotations
    }
}

#[derive(serde::Deserialize)]
struct VerifiedText {
    serial: Serial,
    amount: u64,
    #[serde(flatten)]
    status: NoteStatus,
}

/// Reads the answer of `POST /v1/verify`.
#[wasm_bindgen(js_name = noteReport)]
pub fn note_report(verified: &str) -> Result<NoteReport, JsError> {
    let verified = parse::<VerifiedText>(verified)?;
    let (state, live) = match verified.status {
        NoteStatus::Live {
            position,
            hops_left,
            expires_after_rotations,
        } => ("live", Some((position, hops_left, expires_after_rotations))),
        NoteStatus::Spent => ("spent", None),
        NoteStatus::Expired => ("expired", None),
        NoteStatus::Unissued => ("unissued", None),
    };
    Ok(NoteReport {
        generation: verified.serial.generation,
        index: verified.serial.index,
        amount: verified.amount,
        state,
        position: live.map(|l| l.0),
        hops_left: live.map(|l| l.1),
        expires_after_rotations: live.map(|l| l.2),
    })
}

fn view(info: &str, now: u64) -> Result<MintView, JsError> {
    let info = parse::<MintInfo>(info)?;
    Ok(MintView::new(&info).map_err(error)?.at(now))
}

fn parse<T: DeserializeOwned>(text: &str) -> Result<T, JsError> {
    serde_json::from_str(text).map_err(error)
}

fn json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("protocol objects always serialize")
}

fn error(error: impl std::fmt::Display) -> JsError {
    JsError::new(&error.to_string())
}
