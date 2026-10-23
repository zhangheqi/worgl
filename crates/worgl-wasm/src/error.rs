//! The errors the bindings throw.

use std::fmt::Display;

use wasm_bindgen::JsValue;
use worgl::{NoteError, PublicKey};
use worgl_sdk::{CheckError, LoadError, MAX_REQUESTED_NOTES, NoLayers, PayError, RequestError};

/// Why a call failed. JavaScript gets it as an `Error` named `WorglError`,
/// with a stable `code`, that code's fields in `details`, and the message
/// of the Rust error it comes from.
///
/// The fields are named in camelCase. Amounts and generations are `bigint`,
/// counts and limits `number`, and keys hex strings. The TypeScript SDK
/// lists every code with its fields.
#[derive(Debug)]
pub struct Error {
    code: &'static str,
    details: Vec<(&'static str, Detail)>,
    message: String,
}

/// A field of an error's details.
#[derive(Debug)]
enum Detail {
    /// An amount or a generation.
    Big(u128),
    /// A count or a limit.
    Count(u32),
    /// A key, in hex.
    Key(PublicKey),
}

impl From<u64> for Detail {
    fn from(value: u64) -> Self {
        Self::Big(value.into())
    }
}

impl From<u128> for Detail {
    fn from(value: u128) -> Self {
        Self::Big(value)
    }
}

impl From<u8> for Detail {
    fn from(value: u8) -> Self {
        Self::Count(value.into())
    }
}

impl From<u32> for Detail {
    fn from(value: u32) -> Self {
        Self::Count(value)
    }
}

impl From<PublicKey> for Detail {
    fn from(value: PublicKey) -> Self {
        Self::Key(value)
    }
}

impl Error {
    /// An error of `code` with no fields.
    pub(crate) fn new(code: &'static str, message: impl Display) -> Self {
        Self {
            code,
            details: Vec::new(),
            message: message.to_string(),
        }
    }

    /// The error with one more field.
    fn with(mut self, name: &'static str, value: impl Into<Detail>) -> Self {
        self.details.push((name, value.into()));
        self
    }
}

impl From<Error> for JsValue {
    fn from(error: Error) -> Self {
        let details = js_sys::Object::new();
        for (name, value) in error.details {
            let value = match value {
                Detail::Big(value) => JsValue::from(value),
                Detail::Count(value) => JsValue::from(value),
                Detail::Key(key) => JsValue::from(key.to_string()),
            };
            set(&details, name, &value);
        }
        let thrown = js_sys::Error::new(&error.message);
        thrown.set_name("WorglError");
        set(&thrown, "code", &JsValue::from_str(error.code));
        set(&thrown, "details", &details);
        thrown.into()
    }
}

/// Sets a property of an object made here, which takes any.
fn set(object: &JsValue, name: &str, value: &JsValue) {
    js_sys::Reflect::set(object, &JsValue::from_str(name), value)
        .expect("an ordinary object takes any property");
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self::new("malformed", error)
    }
}

impl From<NoLayers> for Error {
    fn from(error: NoLayers) -> Self {
        Self::new("no_layers", error)
    }
}

impl From<LoadError> for Error {
    fn from(error: LoadError) -> Self {
        let thrown = |code| Self::new(code, &error);
        match &error {
            LoadError::Json(_) => thrown("malformed"),
            LoadError::Format(format) => thrown("unknown_format").with("format", *format),
        }
    }
}

impl From<RequestError> for Error {
    fn from(error: RequestError) -> Self {
        let thrown = |code| Self::new(code, &error);
        match &error {
            RequestError::Zero => thrown("zero_amount"),
            RequestError::Denominations(amount) => thrown("unsplittable").with("amount", *amount),
            RequestError::TooManyNotes => {
                let max = u32::try_from(MAX_REQUESTED_NOTES).unwrap_or(u32::MAX);
                thrown("too_many_notes").with("max", max)
            }
        }
    }
}

impl From<PayError> for Error {
    fn from(error: PayError) -> Self {
        let thrown = |code| Self::new(code, &error);
        match &error {
            PayError::WrongMint => thrown("wrong_mint"),
            PayError::Empty => thrown("empty_request"),
            PayError::Denomination(amount) => thrown("denomination").with("amount", *amount),
            PayError::Paid(holder) => thrown("paid").with("holder", *holder),
            PayError::Unaffordable { spendable, needed } => thrown("unaffordable")
                .with("spendable", *spendable)
                .with("needed", *needed),
            PayError::Fragmented { largest } => thrown("fragmented").with("largest", *largest),
            PayError::TooManyOutputs { max_outputs } => {
                thrown("too_many_outputs").with("maxOutputs", *max_outputs)
            }
            // A bug, and a corrupt store: nothing to act on but the message,
            // which says what failed.
            PayError::Policy(_) => thrown("policy"),
            PayError::Sign(_) => thrown("sign"),
        }
    }
}

impl From<CheckError> for Error {
    fn from(error: CheckError) -> Self {
        let thrown = |code| Self::new(code, &error);
        match &error {
            CheckError::WrongMint => thrown("wrong_mint"),
            CheckError::NotOneInput => thrown("not_one_input"),
            CheckError::Missing(output) => thrown("unpaid")
                .with("amount", output.amount)
                .with("holder", output.holder),
            CheckError::Duplicate(output) => thrown("overpaid")
                .with("amount", output.amount)
                .with("holder", output.holder),
        }
    }
}

impl From<NoteError> for Error {
    fn from(error: NoteError) -> Self {
        let thrown = |code| Self::new(code, &error);
        match &error {
            // Only decoding fails so, and a note that does not decode is
            // refused as malformed JSON before anything checks it.
            NoteError::Malformed(_) | NoteError::Version(_) => thrown("malformed"),
            NoteError::WrongIssuer => thrown("wrong_issuer"),
            NoteError::UnknownGeneration(generation) => {
                thrown("unknown_generation").with("generation", *generation)
            }
            NoteError::BadSignature => thrown("bad_signature"),
        }
    }
}
