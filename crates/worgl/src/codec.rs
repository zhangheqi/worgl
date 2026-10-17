//! A minimal big-endian binary codec for the crate's fixed formats.
//!
//! Notes, transfer messages, events and snapshots all have a stable byte
//! layout that never depends on a serializer's choices, so the bytes that are
//! signed or written to disk are exactly the bytes documented here.

use thiserror::Error;

/// The input ended early or held a value that cannot be decoded.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DecodeError {
    /// The input ended before the value did.
    #[error("input is truncated")]
    Truncated,
    /// Bytes were left over after the value.
    #[error("{0} trailing bytes after the value")]
    Trailing(usize),
    /// A field held a value outside its allowed range.
    #[error("invalid {0}")]
    Invalid(&'static str),
}

#[derive(Default)]
pub(crate) struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            buf: Vec::with_capacity(capacity),
        }
    }

    pub(crate) fn u8(&mut self, v: u8) -> &mut Self {
        self.buf.push(v);
        self
    }

    pub(crate) fn u16(&mut self, v: u16) -> &mut Self {
        self.bytes(&v.to_be_bytes())
    }

    pub(crate) fn u32(&mut self, v: u32) -> &mut Self {
        self.bytes(&v.to_be_bytes())
    }

    pub(crate) fn u64(&mut self, v: u64) -> &mut Self {
        self.bytes(&v.to_be_bytes())
    }

    pub(crate) fn u128(&mut self, v: u128) -> &mut Self {
        self.bytes(&v.to_be_bytes())
    }

    pub(crate) fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(v);
        self
    }

    pub(crate) fn finish(self) -> Vec<u8> {
        self.buf
    }
}

pub(crate) struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }

    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if self.buf.len() < n {
            return Err(DecodeError::Truncated);
        }
        let (head, rest) = self.buf.split_at(n);
        self.buf = rest;
        Ok(head)
    }

    pub(crate) fn array<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.array::<1>()?[0])
    }

    pub(crate) fn u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    pub(crate) fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    pub(crate) fn u64(&mut self) -> Result<u64, DecodeError> {
        Ok(u64::from_be_bytes(self.array()?))
    }

    pub(crate) fn u128(&mut self) -> Result<u128, DecodeError> {
        Ok(u128::from_be_bytes(self.array()?))
    }

    pub(crate) fn remaining(&self) -> usize {
        self.buf.len()
    }

    pub(crate) fn finish(self) -> Result<(), DecodeError> {
        match self.buf.len() {
            0 => Ok(()),
            n => Err(DecodeError::Trailing(n)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_width() {
        let mut w = Writer::default();
        w.u8(1).u16(2).u32(3).u64(4).u128(5).bytes(b"xy");
        let bytes = w.finish();
        let mut r = Reader::new(&bytes);
        assert_eq!(r.u8(), Ok(1));
        assert_eq!(r.u16(), Ok(2));
        assert_eq!(r.u32(), Ok(3));
        assert_eq!(r.u64(), Ok(4));
        assert_eq!(r.u128(), Ok(5));
        assert_eq!(r.take(2), Ok(&b"xy"[..]));
        assert_eq!(r.finish(), Ok(()));
    }

    #[test]
    fn reports_truncation_and_trailing_bytes() {
        assert_eq!(Reader::new(&[0, 1, 2]).u32(), Err(DecodeError::Truncated));
        let r = Reader::new(&[0, 1]);
        assert_eq!(r.finish(), Err(DecodeError::Trailing(2)));
    }
}
