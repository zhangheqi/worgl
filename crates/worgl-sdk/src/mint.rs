//! What a holder knows about a mint, and what that says about each note.

use serde::{Deserialize, Serialize};
use worgl::issuer::LayerSummary;
use worgl::{Note, Policy, PublicKey};

/// A mint's rotation schedule, as its info publishes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    /// Seconds between rotations.
    pub period_secs: u64,
    /// Unix time, in seconds, of the next rotation.
    pub next_at: u64,
}

/// The parts of a mint's `GET /v1/mints/{mint}/info` answer a holder needs.
/// Other fields in the answer are ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MintInfo {
    /// The mint's issuer key, which every one of its notes names.
    pub issuer: PublicKey,
    /// Every layer, first to last.
    pub layers: Vec<LayerSummary>,
    /// The rules the mint enforces.
    pub policy: Policy,
    /// The rotation schedule, or `None` when the mint rotates only on
    /// request.
    #[serde(default)]
    pub rotation: Option<Schedule>,
    /// Whether the mint is paused. A paused mint refuses transfers and skips
    /// its scheduled rotations.
    #[serde(default)]
    pub paused: bool,
}

/// A mint's info has no layers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the mint's info lists no layers")]
pub struct NoLayers;

/// What a holder knows about one mint: enough to tell, without asking it,
/// whether a note can move now and when it dies.
///
/// A note's layer follows from its serial: generations are consecutive, so a
/// note of generation `g` sits at position `g − oldest`. A view holds the
/// layers as the mint's info gave them, and no clock moves it: a note counts
/// as expired only once the info says its layer was dropped. After a
/// rotation, read the info again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MintView {
    /// The mint's issuer key.
    pub issuer: PublicKey,
    /// How many layers the mint has.
    pub layers: u32,
    /// Generation of the first layer.
    pub oldest: u64,
    /// The rules the mint enforces.
    pub policy: Policy,
    /// The rotation schedule, if the mint has one.
    pub schedule: Option<Schedule>,
    /// Whether the mint was paused when the view was read.
    pub paused: bool,
}

/// Where a note stands, as far as a [`MintView`] can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum NoteState {
    /// The note can be spent now.
    Spendable {
        /// Untouched, the note dies in this many rotations from now; it
        /// survives one fewer.
        expires_after_rotations: u32,
    },
    /// The note sits in the last layer and cannot move until the next
    /// rotation.
    Frozen {
        /// Untouched, the note dies in this many rotations from now.
        expires_after_rotations: u32,
    },
    /// The note's layer was dropped, so it is worthless.
    Expired,
    /// The note is newer than the view: read the mint's info again.
    Unknown,
    /// The note belongs to another mint.
    OtherMint,
}

impl MintView {
    /// The view a mint's info gives.
    pub fn new(info: &MintInfo) -> Result<Self, NoLayers> {
        let first = info.layers.first().ok_or(NoLayers)?;
        Ok(Self {
            issuer: info.issuer,
            layers: info.layers.len() as u32,
            oldest: first.generation,
            policy: info.policy.clone(),
            schedule: info.rotation,
            paused: info.paused,
        })
    }

    /// Generation of the last layer, where transferred notes land.
    pub fn newest(&self) -> u64 {
        self.oldest + u64::from(self.layers) - 1
    }

    /// Position of a note's layer, 0 for the first, or `None` if the note
    /// belongs to another mint, has expired or is newer than this view.
    pub fn position(&self, note: &Note) -> Option<u32> {
        let generation = note.serial().generation;
        let mine = note.issuer() == &self.issuer;
        (mine && generation >= self.oldest && generation <= self.newest())
            .then(|| (generation - self.oldest) as u32)
    }

    /// Where a note stands.
    pub fn state(&self, note: &Note) -> NoteState {
        if note.issuer() != &self.issuer {
            return NoteState::OtherMint;
        }
        let generation = note.serial().generation;
        if generation < self.oldest {
            return NoteState::Expired;
        }
        let Some(position) = self.position(note) else {
            return NoteState::Unknown;
        };
        let expires_after_rotations = position + 1;
        if position + 1 < self.layers {
            NoteState::Spendable {
                expires_after_rotations,
            }
        } else {
            NoteState::Frozen {
                expires_after_rotations,
            }
        }
    }

    /// Seconds from Unix time `now` until the next scheduled rotation, if
    /// the mint has a schedule and is not paused. 0 once `now` reaches that
    /// rotation, however far past: as far as the view knows, the mint has
    /// not made it yet, so it is the one about to come.
    pub fn until_rotation(&self, now: u64) -> Option<u64> {
        let schedule = self.schedule.filter(|_| !self.paused)?;
        Some(schedule.next_at.saturating_sub(now))
    }

    /// Unix time, in seconds, of the scheduled rotation that drops a note's
    /// layer, counted from the next rotation the view names. `None` when the
    /// mint has no schedule or is paused, or the note is not live in the
    /// view.
    pub fn expires_at(&self, note: &Note) -> Option<u64> {
        let schedule = self.schedule.filter(|_| !self.paused)?;
        let rotations = match self.state(note) {
            NoteState::Spendable {
                expires_after_rotations,
            }
            | NoteState::Frozen {
                expires_after_rotations,
            } => expires_after_rotations,
            NoteState::Expired | NoteState::Unknown | NoteState::OtherMint => return None,
        };
        let later = u64::from(rotations - 1).saturating_mul(schedule.period_secs);
        Some(schedule.next_at.saturating_add(later))
    }

    /// Unix time, in seconds, of the scheduled rotation after which a
    /// [frozen](NoteState::Frozen) note can move: the next one the view
    /// names. `None` when the mint has no schedule or is paused, or the note
    /// is not frozen or does not outlive that rotation.
    pub fn spendable_at(&self, note: &Note) -> Option<u64> {
        let schedule = self.schedule.filter(|_| !self.paused)?;
        match self.state(note) {
            NoteState::Frozen {
                expires_after_rotations,
            } if expires_after_rotations > 1 => Some(schedule.next_at),
            _ => None,
        }
    }
}
