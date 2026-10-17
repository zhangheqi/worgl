//! Randomness for timing and output order. Keys come from the library.

/// A uniform number in `low..=high`.
pub(crate) fn between(low: u64, high: u64) -> u64 {
    if high <= low {
        return low;
    }
    let span = high - low;
    if span == u64::MAX {
        return word();
    }
    // Rejection sampling keeps the draw uniform.
    let range = span + 1;
    let zone = u64::MAX - (u64::MAX % range);
    loop {
        let w = word();
        if w < zone {
            return low + w % range;
        }
    }
}

/// Shuffles `items` in place.
pub(crate) fn shuffle<T>(items: &mut [T]) {
    for i in (1..items.len()).rev() {
        items.swap(i, between(0, i as u64) as usize);
    }
}

fn word() -> u64 {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).expect("the system RNG failed");
    u64::from_le_bytes(bytes)
}
