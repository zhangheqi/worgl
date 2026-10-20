//! The issuer's layered bit array.
//!
//! One flat bit array of `layers × layer_size` bits is split evenly into
//! layers. Each layer occupies a fixed physical slot and keeps its own count of
//! bits handed out so far (the design's "last used index", plus one). The bit
//! for a note is 1 while the note is live and 0 once it is spent or was never
//! issued.
//!
//! Layers are ordered first (position 0) to last (position `layers − 1`) and
//! each carries a *generation* number that is never reused. Generations are
//! consecutive: the first layer holds generation `oldest`, the last holds
//! `oldest + layers − 1`, and the slot of generation `g` is `g mod layers`.
//! Rotation drops the first layer, wipes its slot in place and reuses it as
//! the new last layer under the next generation, so every other layer moves
//! one position toward the front.
//!
//! A [`Serial`] names a generation rather than a slot. When a slot is reused,
//! a serial from its earlier generation no longer resolves, so an old note can
//! never match a bit that now belongs to a different note. Every operation is
//! O(1) except rotation, which wipes the words the dropped layer used.
//!
//! Bits are handed out in order within a layer. A ledger made with
//! [`Ledger::new`] uses the bit as the serial's index. An [`Issuer`]'s ledger
//! instead maps each bit through a keyed permutation that differs per
//! generation, so indexes do not reveal the order notes were issued in; the
//! key comes from the issuer key and is not part of the encoding.
//!
//! [`Issuer`]: crate::Issuer

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::codec::{DecodeError, Reader, Writer};
use crate::permutation::Permutation;

/// The identity of a note: the generation of the layer it was issued into and
/// its index within that layer, which names one bit of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Serial {
    /// Generation of the layer the note was issued into. Never reused.
    pub generation: u64,
    /// Index of the note within that layer: its bit, or the bit's image
    /// under the issuer's permutation.
    pub index: u32,
}

impl Serial {
    /// Creates a serial.
    pub const fn new(generation: u64, index: u32) -> Self {
        Self { generation, index }
    }
}

impl fmt::Display for Serial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.generation, self.index)
    }
}

/// Text was not a serial of the form `generation:index`.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("a serial is written generation:index")]
pub struct ParseSerialError;

impl FromStr for Serial {
    type Err = ParseSerialError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (generation, index) = s.split_once(':').ok_or(ParseSerialError)?;
        Ok(Self {
            generation: generation.parse().map_err(|_| ParseSerialError)?,
            index: index.parse().map_err(|_| ParseSerialError)?,
        })
    }
}

/// The shape of a ledger, fixed when it is created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerConfig {
    /// Number of layers, at least 2.
    pub layers: u32,
    /// Bits per layer: how many notes can ever be issued into one generation.
    /// A positive multiple of 64.
    pub layer_size: u32,
}

impl LedgerConfig {
    /// Most layers a ledger may have.
    pub const MAX_LAYERS: u32 = 1 << 16;
    /// Most bits a whole ledger may hold (8 GiB of bitmap).
    pub const MAX_TOTAL_BITS: u64 = 1 << 36;

    /// Checks the limits documented on each field.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !(2..=Self::MAX_LAYERS).contains(&self.layers) {
            return Err(ConfigError::Layers(self.layers));
        }
        if self.layer_size == 0 || !self.layer_size.is_multiple_of(64) {
            return Err(ConfigError::LayerSize(self.layer_size));
        }
        if u64::from(self.layers) * u64::from(self.layer_size) > Self::MAX_TOTAL_BITS {
            return Err(ConfigError::TooLarge);
        }
        Ok(())
    }
}

/// A [`LedgerConfig`] outside the allowed limits.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ConfigError {
    /// Fewer than 2 or more than [`LedgerConfig::MAX_LAYERS`] layers.
    #[error("a ledger needs 2 to {max} layers, not {0}", max = LedgerConfig::MAX_LAYERS)]
    Layers(u32),
    /// The layer size is zero or not a multiple of 64.
    #[error("layer size must be a positive multiple of 64, not {0}")]
    LayerSize(u32),
    /// The whole bitmap would exceed [`LedgerConfig::MAX_TOTAL_BITS`].
    #[error("ledger would exceed {max} bits", max = LedgerConfig::MAX_TOTAL_BITS)]
    TooLarge,
}

/// What the ledger knows about a serial.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Status {
    /// Issued and not yet spent; its layer is at `position`.
    Live {
        /// Position of the note's layer, 0 for the first.
        position: u32,
    },
    /// Issued and then spent.
    Spent,
    /// Its layer has been dropped by a rotation.
    Expired,
    /// The ledger has never issued this serial.
    Unissued,
}

/// A ledger operation that could not be carried out.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LedgerError {
    /// There is no layer at this position.
    #[error("no layer at position {0}")]
    NoSuchPosition(u32),
    /// Every bit of the layer has been handed out for this generation.
    #[error("layer at position {position} is full until the next rotation")]
    LayerFull {
        /// Position of the full layer.
        position: u32,
    },
    /// The serial is not live, so it cannot be invalidated.
    #[error("note {serial} is not live ({status:?})")]
    NotLive {
        /// The serial that was to be invalidated.
        serial: Serial,
        /// Its actual status.
        status: Status,
    },
}

/// The outcome of a [`Ledger::rotate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rotation {
    /// Generation of the layer that was dropped.
    pub dropped: u64,
    /// Live notes the dropped layer still held; they are now expired.
    pub destroyed: u32,
    /// Generation of the new last layer.
    pub created: u64,
}

/// Usage of one layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerInfo {
    /// Position, 0 for the first layer.
    pub position: u32,
    /// The layer's generation.
    pub generation: u64,
    /// Bits handed out so far in this generation.
    pub issued: u32,
    /// Notes in this layer that are still live.
    pub live: u32,
    /// Bits the layer has in total.
    pub capacity: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Slot {
    /// Bits handed out in the current generation; the next index to issue.
    next: u32,
    /// Bits currently set.
    live: u32,
}

/// The layered bit array. See the [module documentation](self).
#[derive(Clone, PartialEq, Eq)]
pub struct Ledger {
    config: LedgerConfig,
    /// Generation of the first layer.
    oldest: u64,
    /// `layers × layer_size` bits, slot-major.
    words: Vec<u64>,
    /// Per-slot counters, indexed by slot.
    slots: Vec<Slot>,
    /// Maps bits to serial indexes; `None` uses the bit itself.
    order: Option<Permutation>,
}

impl fmt::Debug for Ledger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ledger")
            .field("config", &self.config)
            .field("oldest", &self.oldest)
            .field("permuted", &self.order.is_some())
            .field("layers", &self.layers().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl Ledger {
    /// Creates an empty ledger whose layers hold generations `0..layers`.
    /// Serial indexes are the bits themselves.
    pub fn new(config: LedgerConfig) -> Result<Self, ConfigError> {
        config.validate()?;
        let words = usize::try_from(Self::words_per_layer(config) * u64::from(config.layers))
            .map_err(|_| ConfigError::TooLarge)?;
        Ok(Self {
            config,
            oldest: 0,
            words: vec![0; words],
            slots: vec![Slot::default(); config.layers as usize],
            order: None,
        })
    }

    /// Maps bits to serial indexes through `order` from now on. Only an
    /// empty ledger or one restored from a snapshot that used the same
    /// permutation may be given one.
    pub(crate) fn with_order(mut self, order: Permutation) -> Self {
        self.order = Some(order);
        self
    }

    /// Whether serial indexes are a permutation of the bits.
    pub fn is_permuted(&self) -> bool {
        self.order.is_some()
    }

    /// The ledger's shape.
    pub fn config(&self) -> LedgerConfig {
        self.config
    }

    /// Position of the last layer, whose notes cannot be transferred.
    pub fn last_position(&self) -> u32 {
        self.config.layers - 1
    }

    /// Generation of the first layer.
    pub fn oldest_generation(&self) -> u64 {
        self.oldest
    }

    /// Generation of the last layer.
    pub fn newest_generation(&self) -> u64 {
        self.oldest + u64::from(self.last_position())
    }

    /// Current position of a generation, or `None` if it was dropped or does not exist yet.
    pub fn position(&self, generation: u64) -> Option<u32> {
        let offset = generation.checked_sub(self.oldest)?;
        (offset < u64::from(self.config.layers)).then_some(offset as u32)
    }

    /// Generation of the layer at a position.
    pub fn generation_at(&self, position: u32) -> Option<u64> {
        (position < self.config.layers).then(|| self.oldest + u64::from(position))
    }

    /// What the ledger knows about a serial. O(1).
    pub fn status(&self, serial: Serial) -> Status {
        self.locate(serial).0
    }

    /// Whether a serial is live. O(1).
    pub fn is_live(&self, serial: Serial) -> bool {
        matches!(self.status(serial), Status::Live { .. })
    }

    /// Bits not yet handed out in the layer at a position.
    pub fn free(&self, position: u32) -> Option<u32> {
        let generation = self.generation_at(position)?;
        Some(self.config.layer_size - self.slots[self.slot_of(generation)].next)
    }

    /// The serial the `k`-th next allocation in the layer at `position` would
    /// get, or `None` if the layer has fewer than `k + 1` free bits. O(1).
    pub fn next_serial(&self, position: u32, k: u32) -> Option<Serial> {
        let generation = self.generation_at(position)?;
        let bit = self.slots[self.slot_of(generation)].next.checked_add(k)?;
        (bit < self.config.layer_size).then(|| self.serial_of(generation, bit))
    }

    /// Sets the next unused bit of the layer at `position` and returns its serial. O(1).
    pub fn allocate(&mut self, position: u32) -> Result<Serial, LedgerError> {
        let generation = self
            .generation_at(position)
            .ok_or(LedgerError::NoSuchPosition(position))?;
        let slot = self.slot_of(generation);
        let bit = self.slots[slot].next;
        if bit >= self.config.layer_size {
            return Err(LedgerError::LayerFull { position });
        }
        self.set_bit(slot, bit, true);
        self.slots[slot].next += 1;
        self.slots[slot].live += 1;
        Ok(self.serial_of(generation, bit))
    }

    /// Clears a live note's bit and returns the position of its layer. O(1).
    pub fn invalidate(&mut self, serial: Serial) -> Result<u32, LedgerError> {
        match self.locate(serial) {
            (Status::Live { position }, bit) => {
                let slot = self.slot_of(serial.generation);
                self.set_bit(slot, bit, false);
                self.slots[slot].live -= 1;
                Ok(position)
            }
            (status, _) => Err(LedgerError::NotLive { serial, status }),
        }
    }

    /// Drops the first layer and reuses its slot, wiped, as the new last layer.
    ///
    /// Costs one word write per 64 bits the dropped layer had handed out.
    ///
    /// # Panics
    ///
    /// If the generation counter would overflow `u64`, which takes 2⁶⁴ rotations.
    pub fn rotate(&mut self) -> Rotation {
        let dropped = self.oldest;
        let created = self
            .newest_generation()
            .checked_add(1)
            .expect("generation counter overflowed");
        let slot = self.slot_of(dropped);
        let destroyed = self.slots[slot].live;
        let used_words = self.slots[slot].next.div_ceil(64) as usize;
        let start = slot * self.words_per_slot();
        self.words[start..start + used_words].fill(0);
        self.slots[slot] = Slot::default();
        self.oldest += 1;
        debug_assert_eq!(self.slot_of(created), slot);
        Rotation {
            dropped,
            destroyed,
            created,
        }
    }

    /// Usage of the layer at a position.
    pub fn layer(&self, position: u32) -> Option<LayerInfo> {
        let generation = self.generation_at(position)?;
        let slot = self.slots[self.slot_of(generation)];
        Some(LayerInfo {
            position,
            generation,
            issued: slot.next,
            live: slot.live,
            capacity: self.config.layer_size,
        })
    }

    /// Usage of every layer, first to last.
    pub fn layers(&self) -> impl Iterator<Item = LayerInfo> + '_ {
        (0..self.config.layers).filter_map(|position| self.layer(position))
    }

    /// Live notes across all layers.
    pub fn live(&self) -> u64 {
        self.slots.iter().map(|s| u64::from(s.live)).sum()
    }

    /// Encodes the ledger. Only words that can hold set bits are written, so a
    /// lightly used ledger encodes small.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = Writer::default();
        self.encode(&mut w);
        w.finish()
    }

    /// Decodes a ledger written by [`to_bytes`](Self::to_bytes) and checks
    /// that its counters agree with its bits.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut r = Reader::new(bytes);
        let ledger = Self::decode(&mut r)?;
        r.finish()?;
        Ok(ledger)
    }

    pub(crate) fn encode(&self, w: &mut Writer) {
        w.u32(self.config.layers)
            .u32(self.config.layer_size)
            .u64(self.oldest);
        let per_slot = self.words_per_slot();
        for (slot, counters) in self.slots.iter().enumerate() {
            w.u32(counters.next).u32(counters.live);
            let start = slot * per_slot;
            let used = counters.next.div_ceil(64) as usize;
            for word in &self.words[start..start + used] {
                w.u64(*word);
            }
        }
    }

    pub(crate) fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let config = LedgerConfig {
            layers: r.u32()?,
            layer_size: r.u32()?,
        };
        let oldest = r.u64()?;
        // Check the claimed size against the input before allocating for it.
        config
            .validate()
            .map_err(|_| DecodeError::Invalid("ledger shape"))?;
        if oldest.checked_add(u64::from(config.layers)).is_none() {
            return Err(DecodeError::Invalid("generation"));
        }
        if r.remaining() < config.layers as usize * 8 {
            return Err(DecodeError::Truncated);
        }
        let mut ledger = Self::new(config).map_err(|_| DecodeError::Invalid("ledger shape"))?;
        ledger.oldest = oldest;
        let per_slot = ledger.words_per_slot();
        for slot in 0..config.layers as usize {
            let next = r.u32()?;
            let live = r.u32()?;
            if next > config.layer_size || live > next {
                return Err(DecodeError::Invalid("layer counters"));
            }
            let start = slot * per_slot;
            let used = next.div_ceil(64) as usize;
            let mut popcount = 0;
            for word in &mut ledger.words[start..start + used] {
                *word = r.u64()?;
                popcount += word.count_ones();
            }
            // Bits at or past `next` were never handed out and must be clear.
            if !next.is_multiple_of(64) && ledger.words[start + used - 1] >> (next % 64) != 0 {
                return Err(DecodeError::Invalid("bits past the last used index"));
            }
            if popcount != live {
                return Err(DecodeError::Invalid("live count"));
            }
            ledger.slots[slot] = Slot { next, live };
        }
        Ok(ledger)
    }

    fn words_per_layer(config: LedgerConfig) -> u64 {
        u64::from(config.layer_size / 64)
    }

    fn words_per_slot(&self) -> usize {
        (self.config.layer_size / 64) as usize
    }

    /// The serial that names `bit` of `generation`'s layer, whether or not
    /// the bit was handed out. `bit` must be below the layer size.
    pub(crate) fn serial_of(&self, generation: u64, bit: u32) -> Serial {
        let index = match &self.order {
            Some(order) => order.index(generation, bit),
            None => bit,
        };
        Serial { generation, index }
    }

    /// The bit a serial names in its layer, or `None` if its generation is
    /// not in the ledger or its index is past the layer's size.
    pub(crate) fn bit_of(&self, serial: Serial) -> Option<u32> {
        self.position(serial.generation)?;
        if serial.index >= self.config.layer_size {
            return None;
        }
        Some(match &self.order {
            Some(order) => order.bit(serial.generation, serial.index),
            None => serial.index,
        })
    }

    /// A serial's status and, if it is issued, its bit.
    fn locate(&self, serial: Serial) -> (Status, u32) {
        if serial.generation < self.oldest {
            return (Status::Expired, 0);
        }
        let (Some(position), Some(bit)) = (self.position(serial.generation), self.bit_of(serial))
        else {
            return (Status::Unissued, 0);
        };
        let slot = self.slot_of(serial.generation);
        let status = if bit >= self.slots[slot].next {
            Status::Unissued
        } else if self.bit(slot, bit) {
            Status::Live { position }
        } else {
            Status::Spent
        };
        (status, bit)
    }

    fn slot_of(&self, generation: u64) -> usize {
        (generation % u64::from(self.config.layers)) as usize
    }

    fn bit(&self, slot: usize, index: u32) -> bool {
        let word = self.words[slot * self.words_per_slot() + (index / 64) as usize];
        word >> (index % 64) & 1 == 1
    }

    fn set_bit(&mut self, slot: usize, index: u32, value: bool) {
        let i = slot * self.words_per_slot() + (index / 64) as usize;
        let mask = 1u64 << (index % 64);
        if value {
            self.words[i] |= mask;
        } else {
            self.words[i] &= !mask;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger(layers: u32, layer_size: u32) -> Ledger {
        Ledger::new(LedgerConfig { layers, layer_size }).unwrap()
    }

    #[test]
    fn rejects_bad_shapes() {
        let bad = [
            (1, 64),
            (4, 0),
            (4, 100),
            (LedgerConfig::MAX_LAYERS + 1, 64),
        ];
        for (layers, layer_size) in bad {
            assert!(Ledger::new(LedgerConfig { layers, layer_size }).is_err());
        }
        assert_eq!(
            Ledger::new(LedgerConfig {
                layers: 1 << 16,
                layer_size: 1 << 21
            })
            .unwrap_err(),
            ConfigError::TooLarge
        );
    }

    #[test]
    fn allocates_sequentially_per_layer() {
        let mut l = ledger(4, 64);
        assert_eq!(l.allocate(2), Ok(Serial::new(2, 0)));
        assert_eq!(l.allocate(2), Ok(Serial::new(2, 1)));
        assert_eq!(l.allocate(0), Ok(Serial::new(0, 0)));
        assert_eq!(l.status(Serial::new(2, 1)), Status::Live { position: 2 });
        assert_eq!(l.status(Serial::new(2, 2)), Status::Unissued);
        assert_eq!(l.status(Serial::new(9, 0)), Status::Unissued);
        assert_eq!(l.free(2), Some(62));
        assert_eq!(l.allocate(4), Err(LedgerError::NoSuchPosition(4)));
    }

    #[test]
    fn a_permuted_ledger_hands_out_bits_in_order_under_scrambled_indexes() {
        let mut l = ledger(2, 64).with_order(Permutation::new([1; 32], 64));
        assert!(l.is_permuted());
        let serials: Vec<Serial> = (0..64)
            .map(|k| {
                let next = l.next_serial(0, 0);
                assert_eq!(l.next_serial(0, 64 - k), None);
                let serial = l.allocate(0).unwrap();
                assert_eq!(Some(serial), next);
                serial
            })
            .collect();
        assert_ne!(serials[1].index, serials[0].index + 1);
        let mut indexes: Vec<u32> = serials.iter().map(|s| s.index).collect();
        indexes.sort_unstable();
        assert_eq!(indexes, (0..64).collect::<Vec<_>>());
        assert_eq!(l.allocate(0), Err(LedgerError::LayerFull { position: 0 }));

        assert_eq!(l.invalidate(serials[5]), Ok(0));
        assert_eq!(l.status(serials[5]), Status::Spent);
        assert_eq!(l.status(serials[6]), Status::Live { position: 0 });
        assert_eq!(l.status(Serial::new(0, 64)), Status::Unissued);
        // Each layer still counts its own bits.
        assert_eq!(l.layer(0).unwrap().live, 63);
        let other = l.allocate(1).unwrap();
        assert_eq!(l.status(other), Status::Live { position: 1 });
    }

    #[test]
    fn invalidates_once() {
        let mut l = ledger(2, 64);
        let s = l.allocate(0).unwrap();
        assert_eq!(l.invalidate(s), Ok(0));
        assert_eq!(l.status(s), Status::Spent);
        assert_eq!(
            l.invalidate(s),
            Err(LedgerError::NotLive {
                serial: s,
                status: Status::Spent
            })
        );
    }

    #[test]
    fn full_layer_refuses_until_rotated_out() {
        let mut l = ledger(2, 64);
        for _ in 0..64 {
            l.allocate(1).unwrap();
        }
        assert_eq!(l.allocate(1), Err(LedgerError::LayerFull { position: 1 }));
        // Rotation moves the full layer to position 0 and opens a fresh last layer.
        l.rotate();
        assert_eq!(l.allocate(0), Err(LedgerError::LayerFull { position: 0 }));
        assert_eq!(l.allocate(1), Ok(Serial::new(2, 0)));
    }

    #[test]
    fn rotation_expires_the_first_layer_and_reuses_its_slot() {
        let mut l = ledger(3, 64);
        let old = l.allocate(0).unwrap();
        let kept = l.allocate(1).unwrap();
        let r = l.rotate();
        assert_eq!(
            r,
            Rotation {
                dropped: 0,
                destroyed: 1,
                created: 3
            }
        );
        assert_eq!(l.status(old), Status::Expired);
        assert_eq!(l.status(kept), Status::Live { position: 0 });
        // Slot 0 is reused for generation 3; index 0 there is a different note.
        let fresh = l.allocate(2).unwrap();
        assert_eq!(fresh, Serial::new(3, 0));
        assert_eq!(l.status(old), Status::Expired);
        assert_eq!(l.status(fresh), Status::Live { position: 2 });
    }

    #[test]
    fn serial_text_round_trips() {
        let s = Serial::new(12, 345);
        assert_eq!(s.to_string(), "12:345");
        assert_eq!("12:345".parse(), Ok(s));
        assert!("12".parse::<Serial>().is_err());
        assert!("a:1".parse::<Serial>().is_err());
    }

    #[test]
    fn encoding_round_trips_and_rejects_inconsistent_counters() {
        let mut l = ledger(3, 128);
        for _ in 0..70 {
            l.allocate(1).unwrap();
        }
        l.invalidate(Serial::new(1, 5)).unwrap();
        l.rotate();
        let bytes = l.to_bytes();
        assert_eq!(Ledger::from_bytes(&bytes).unwrap(), l);

        // Header (16) + slot 0 counters (8) + nothing, then slot 1's live count.
        let mut tampered = bytes.clone();
        tampered[16 + 8 + 4 + 3] ^= 1;
        assert!(Ledger::from_bytes(&tampered).is_err());
        assert!(Ledger::from_bytes(&bytes[..bytes.len() - 1]).is_err());
    }
}
