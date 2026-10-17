//! Fixed-size byte arrays as hex in human-readable formats and raw bytes otherwise.

use std::fmt;
use std::marker::PhantomData;

use serde::de::{self, Visitor};
use serde::{Deserializer, Serializer};

pub(crate) fn parse_hex<const N: usize>(s: &str) -> Option<[u8; N]> {
    let mut out = [0u8; N];
    hex::decode_to_slice(s, &mut out).ok()?;
    Some(out)
}

pub(crate) fn serialize<S: Serializer, const N: usize>(
    bytes: &[u8; N],
    s: S,
) -> Result<S::Ok, S::Error> {
    if s.is_human_readable() {
        s.serialize_str(&hex::encode(bytes))
    } else {
        s.serialize_bytes(bytes)
    }
}

pub(crate) fn deserialize<'de, D: Deserializer<'de>, const N: usize>(
    d: D,
) -> Result<[u8; N], D::Error> {
    if d.is_human_readable() {
        d.deserialize_str(FixedVisitor::<N>(PhantomData))
    } else {
        d.deserialize_bytes(FixedVisitor::<N>(PhantomData))
    }
}

struct FixedVisitor<const N: usize>(PhantomData<[u8; N]>);

impl<'de, const N: usize> Visitor<'de> for FixedVisitor<N> {
    type Value = [u8; N];

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{N} bytes, or {} hex digits", 2 * N)
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
        parse_hex(v).ok_or_else(|| E::invalid_value(de::Unexpected::Str(v), &self))
    }

    fn visit_bytes<E: de::Error>(self, v: &[u8]) -> Result<Self::Value, E> {
        v.try_into().map_err(|_| E::invalid_length(v.len(), &self))
    }

    fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut out = [0u8; N];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = seq
                .next_element()?
                .ok_or_else(|| de::Error::invalid_length(i, &self))?;
        }
        if seq.next_element::<u8>()?.is_some() {
            return Err(de::Error::invalid_length(N + 1, &self));
        }
        Ok(out)
    }
}
