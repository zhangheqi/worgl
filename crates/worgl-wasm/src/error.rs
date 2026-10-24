//! The errors the bindings throw.

use std::fmt::Display;

use wasm_bindgen::JsValue;
use worgl::{NoteError, PublicKey};
use worgl_sdk::vault::VaultError;
use worgl_sdk::{
    AnswerError, CheckError, LoadError, MAX_REQUESTED_NOTES, NoLayers, PayError, RequestError,
};

/// Why a call failed. JavaScript gets it as an `Error` named `WorglError`,
/// with a stable `code`, that code's fields in `details`, and the message
/// of the Rust error it comes from.
///
/// The fields are named in camelCase. Amounts and generations are `bigint`,
/// counts and limits `number`, keys hex strings, and a field with no value
/// `undefined`. The TypeScript SDK lists every code with its fields.
#[derive(Debug)]
pub struct Error {
    code: &'static str,
    details: Vec<(&'static str, Detail)>,
    message: String,
}

/// A field of an error's details.
#[derive(Debug, PartialEq, Eq)]
enum Detail {
    /// An amount or a generation.
    Big(u128),
    /// A count or a limit.
    Count(u32),
    /// A key, in hex.
    Key(PublicKey),
    /// No value: `undefined`.
    Undefined,
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

impl<T: Into<Detail>> From<Option<T>> for Detail {
    fn from(value: Option<T>) -> Self {
        value.map_or(Self::Undefined, Into::into)
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
                Detail::Undefined => JsValue::UNDEFINED,
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

impl From<VaultError> for Error {
    fn from(error: VaultError) -> Self {
        let thrown = |code| Self::new(code, &error);
        match &error {
            VaultError::NotSealed => thrown("not_sealed"),
            VaultError::Version(version) => thrown("unknown_version").with("version", *version),
            // The file opened under its key, so whoever sealed it wrote a
            // holder no SDK reads: as damaged as a file altered after.
            VaultError::Damaged(_) | VaultError::Holder(LoadError::Json(_)) => thrown("damaged"),
            VaultError::Holder(LoadError::Format(format)) => {
                thrown("unknown_format").with("format", *format)
            }
            VaultError::Passphrase => thrown("wrong_passphrase"),
            VaultError::EmptyPassphrase => thrown("empty_passphrase"),
            VaultError::Memory => thrown("out_of_memory"),
            VaultError::Random => thrown("no_randomness"),
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
            PayError::Fragmented { largest, at_most } => thrown("fragmented")
                .with("largest", *largest)
                .with("atMost", *at_most),
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

impl From<AnswerError> for Error {
    fn from(error: AnswerError) -> Self {
        let thrown = |code| Self::new(code, &error);
        let count = |value: usize| u32::try_from(value).unwrap_or(u32::MAX);
        match &error {
            // As for any transfer that spends no notes.
            AnswerError::NoInputs => thrown("malformed"),
            // The transfer's notes are of another mint than the keys.
            AnswerError::WrongKeys => thrown("wrong_issuer"),
            AnswerError::Count { outputs, answered } => thrown("miscounted")
                .with("outputs", count(*outputs))
                .with("answered", count(*answered)),
            AnswerError::Unasked(index) => thrown("unasked").with("index", count(*index)),
            AnswerError::Note { index, .. } => thrown("unsigned").with("index", count(*index)),
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

#[cfg(test)]
mod tests {
    use worgl::{HolderKey, Note, Output, PolicyError};

    use super::*;

    /// Checks the code and fields `error` is thrown with, and that its
    /// message is the Rust error's.
    fn check<E: Display + Into<Error>>(error: E, code: &str, details: &[(&'static str, Detail)]) {
        let message = error.to_string();
        let thrown: Error = error.into();
        assert_eq!(thrown.code, code, "{message}");
        assert_eq!(thrown.details, details, "{message}");
        assert_eq!(thrown.message, message);
    }

    #[test]
    fn requests_are_refused_with_their_codes() {
        check(RequestError::Zero, "zero_amount", &[]);
        let amount = ("amount", Detail::Big(5));
        check(RequestError::Denominations(5), "unsplittable", &[amount]);
        let max = ("max", Detail::Count(256));
        check(RequestError::TooManyNotes, "too_many_notes", &[max]);
    }

    #[test]
    fn payments_are_refused_with_their_codes() {
        let key = HolderKey::generate().public_key();
        check(PayError::WrongMint, "wrong_mint", &[]);
        check(PayError::Empty, "empty_request", &[]);
        let amount = ("amount", Detail::Big(3));
        check(PayError::Denomination(3), "denomination", &[amount]);
        check(PayError::Paid(key), "paid", &[("holder", Detail::Key(key))]);
        let unaffordable = PayError::Unaffordable {
            spendable: 8,
            needed: 16,
        };
        let fields = [("spendable", Detail::Big(8)), ("needed", Detail::Big(16))];
        check(unaffordable, "unaffordable", &fields);
        let fragmented = PayError::Fragmented {
            largest: 4,
            at_most: Some(1),
        };
        let fields = [("largest", Detail::Big(4)), ("atMost", Detail::Big(1))];
        check(fragmented, "fragmented", &fields);
        let fragmented = PayError::Fragmented {
            largest: 64,
            at_most: None,
        };
        let fields = [("largest", Detail::Big(64)), ("atMost", Detail::Undefined)];
        check(fragmented, "fragmented", &fields);
        let outputs = PayError::TooManyOutputs { max_outputs: 16 };
        check(
            outputs,
            "too_many_outputs",
            &[("maxOutputs", Detail::Count(16))],
        );
        check(PayError::Policy(PolicyError::Limits), "policy", &[]);
        let sign = PayError::Sign(PolicyError::Limits.into());
        check(sign, "sign", &[]);
    }

    #[test]
    fn payments_that_do_not_match_their_request_have_codes() {
        let holder = HolderKey::generate().public_key();
        let output = Output { amount: 4, holder };
        let fields = [("amount", Detail::Big(4)), ("holder", Detail::Key(holder))];
        check(CheckError::WrongMint, "wrong_mint", &[]);
        check(CheckError::NotOneInput, "not_one_input", &[]);
        check(CheckError::Missing(output), "unpaid", &fields);
        check(CheckError::Duplicate(output), "overpaid", &fields);
    }

    #[test]
    fn answers_that_are_not_the_mints_have_codes() {
        check(AnswerError::NoInputs, "malformed", &[]);
        check(AnswerError::WrongKeys, "wrong_issuer", &[]);
        let count = AnswerError::Count {
            outputs: 4,
            answered: 3,
        };
        let fields = [
            ("outputs", Detail::Count(4)),
            ("answered", Detail::Count(3)),
        ];
        check(count, "miscounted", &fields);
        let index = [("index", Detail::Count(1))];
        check(AnswerError::Unasked(1), "unasked", &index);
        let unsigned = AnswerError::Note {
            index: 1,
            error: NoteError::BadSignature,
        };
        check(unsigned, "unsigned", &index);
    }

    #[test]
    fn notes_that_do_not_verify_have_codes() {
        let truncated = Note::from_bytes(&[]).unwrap_err();
        assert!(matches!(truncated, NoteError::Malformed(_)));
        check(truncated, "malformed", &[]);
        check(NoteError::Version(9), "malformed", &[]);
        check(NoteError::WrongIssuer, "wrong_issuer", &[]);
        let generation = ("generation", Detail::Big(7));
        check(
            NoteError::UnknownGeneration(7),
            "unknown_generation",
            &[generation],
        );
        check(NoteError::BadSignature, "bad_signature", &[]);
    }

    #[test]
    fn unreadable_arguments_have_codes() {
        let json = || serde_json::from_str::<u8>("{").unwrap_err();
        check(json(), "malformed", &[]);
        check(LoadError::Json(json()), "malformed", &[]);
        let format = ("format", Detail::Count(9));
        check(LoadError::Format(9), "unknown_format", &[format]);
        check(NoLayers, "no_layers", &[]);
    }

    #[test]
    fn purse_files_that_do_not_open_or_seal_have_codes() {
        check(VaultError::NotSealed, "not_sealed", &[]);
        let version = ("version", Detail::Count(2));
        check(VaultError::Version(2), "unknown_version", &[version]);
        check(VaultError::Damaged("nonce"), "damaged", &[]);
        let json = serde_json::from_str::<u8>("{").unwrap_err();
        check(VaultError::Holder(LoadError::Json(json)), "damaged", &[]);
        let format = ("format", Detail::Count(9));
        let newer = VaultError::Holder(LoadError::Format(9));
        check(newer, "unknown_format", &[format]);
        check(VaultError::Passphrase, "wrong_passphrase", &[]);
        check(VaultError::EmptyPassphrase, "empty_passphrase", &[]);
        check(VaultError::Memory, "out_of_memory", &[]);
        check(VaultError::Random, "no_randomness", &[]);
    }
}
