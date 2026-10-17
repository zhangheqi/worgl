//! A keyed permutation of the bits of a layer, so serial indexes do not
//! reveal the order notes were issued in.
//!
//! The ledger hands out a layer's bits in order, which keeps allocation O(1)
//! and the encoding compact. Without a permutation a note's index would be its
//! bit, so anyone holding two notes of one generation could tell which was
//! issued first, and the outputs of one split would carry consecutive indexes.
//! With it, the index is a pseudorandom function of the bit, different in
//! every generation, and only the issuer can map one to the other.
//!
//! The construction is a balanced Feistel network over the smallest even
//! number of bits that covers the layer, with ten rounds as in NIST's FF1 and
//! SHA-256 of the key, the generation, the round and the half block as the
//! round function. A result outside the layer is enciphered again (cycle
//! walking), which ends because the permutation's cycle through the starting
//! bit returns to the layer. The covering domain is less than four times the
//! layer, so this takes under four passes on average, and exactly one when
//! the layer size is a power of four.

use sha2::{Digest, Sha256};

use crate::keys::IssuerKey;

const ROUNDS: u8 = 10;
const KEY_DOMAIN: &[u8] = b"worgl/serial-order/v1";

/// A permutation of `0..size` for each generation.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Permutation {
    key: [u8; 32],
    size: u32,
    half_bits: u32,
}

impl Permutation {
    /// The permutation an issuer key implies for layers of `size` bits.
    ///
    /// The key is derived from the issuer's secret rather than stored, so it
    /// lives wherever the issuer key does and never in a snapshot.
    pub(crate) fn for_issuer(key: &IssuerKey, size: u32) -> Self {
        let mut secret = key.to_secret_bytes();
        let derived = Sha256::new()
            .chain_update(KEY_DOMAIN)
            .chain_update(secret)
            .finalize()
            .into();
        secret.fill(0);
        Self::new(derived, size)
    }

    pub(crate) fn new(key: [u8; 32], size: u32) -> Self {
        assert!(size >= 2, "a layer has at least two bits");
        let bits = u32::BITS - (size - 1).leading_zeros();
        Self {
            key,
            size,
            half_bits: bits.div_ceil(2),
        }
    }

    /// The index the bit gets in the given generation.
    pub(crate) fn index(&self, generation: u64, bit: u32) -> u32 {
        debug_assert!(bit < self.size);
        let mut x = bit;
        loop {
            let (mut left, mut right) = self.split(x);
            for round in 0..ROUNDS {
                (left, right) = (right, left ^ self.round(generation, round, right));
            }
            x = self.join(left, right);
            if x < self.size {
                return x;
            }
        }
    }

    /// The bit an index stands for in the given generation; the inverse of
    /// [`index`](Self::index).
    pub(crate) fn bit(&self, generation: u64, index: u32) -> u32 {
        debug_assert!(index < self.size);
        let mut x = index;
        loop {
            let (mut left, mut right) = self.split(x);
            for round in (0..ROUNDS).rev() {
                (left, right) = (right ^ self.round(generation, round, left), left);
            }
            x = self.join(left, right);
            if x < self.size {
                return x;
            }
        }
    }

    fn mask(&self) -> u32 {
        (1 << self.half_bits) - 1
    }

    fn split(&self, x: u32) -> (u32, u32) {
        (x >> self.half_bits, x & self.mask())
    }

    fn join(&self, left: u32, right: u32) -> u32 {
        left << self.half_bits | right
    }

    fn round(&self, generation: u64, round: u8, half: u32) -> u32 {
        // 45 bytes: one SHA-256 block.
        let digest = Sha256::new()
            .chain_update(self.key)
            .chain_update(generation.to_be_bytes())
            .chain_update([round])
            .chain_update(half.to_be_bytes())
            .finalize();
        u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) & self.mask()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn permutation(size: u32) -> Permutation {
        Permutation::new([7; 32], size)
    }

    #[test]
    fn every_bit_gets_a_distinct_index_and_maps_back() {
        // 64 and 4096 cover an even number of bits, 192 and 1088 need cycle walking.
        for size in [64, 192, 1088, 4096] {
            let p = permutation(size);
            for generation in [0, 1, u64::MAX] {
                let mut seen = vec![false; size as usize];
                for bit in 0..size {
                    let index = p.index(generation, bit);
                    assert!(index < size);
                    assert!(!seen[index as usize], "index {index} given twice");
                    seen[index as usize] = true;
                    assert_eq!(p.bit(generation, index), bit);
                }
            }
        }
    }

    #[test]
    fn indexes_hide_issue_order() {
        let p = permutation(1 << 16);
        let first: Vec<u32> = (0..16).map(|bit| p.index(5, bit)).collect();
        assert_ne!(first, (0..16).collect::<Vec<_>>());
        // Consecutive bits rarely land next to each other.
        let adjacent = first
            .windows(2)
            .filter(|w| w[0].abs_diff(w[1]) <= 1)
            .count();
        assert!(adjacent <= 1, "{first:?}");
        // Each generation and each key orders its bits differently.
        let other_generation: Vec<u32> = (0..16).map(|bit| p.index(6, bit)).collect();
        assert_ne!(first, other_generation);
        let other_key = Permutation::new([8; 32], 1 << 16);
        assert_ne!(
            first,
            (0..16)
                .map(|bit| other_key.index(5, bit))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn the_issuer_key_fixes_the_permutation() {
        let key = IssuerKey::generate();
        let again = IssuerKey::from_secret_bytes(&key.to_secret_bytes());
        assert!(Permutation::for_issuer(&key, 64) == Permutation::for_issuer(&again, 64));
        assert!(
            Permutation::for_issuer(&key, 64)
                != Permutation::for_issuer(&IssuerKey::generate(), 64)
        );
    }
}
