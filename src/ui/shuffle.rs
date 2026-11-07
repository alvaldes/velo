//! Shuffle (playback-and-library T2): the order of the queue entries still to
//! play, mixed and put back. Pure, so the rules are tested without a device;
//! the interface applies them through its one queue edit, which is how they
//! reach the player.

use std::path::PathBuf;

/// Mixes `items[keep..]` in place — a Fisher–Yates over a xorshift sequence
/// started from `seed` — and leaves `items[..keep]`, what played and what
/// plays now, exactly where it was.
pub fn shuffle_after<T>(items: &mut [T], keep: usize, seed: u64) {
    let Some(rest) = items.get_mut(keep..) else {
        return;
    };
    let mut state = seed | 1;
    for last in (1..rest.len()).rev() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let bound = u64::try_from(last + 1).unwrap_or(u64::MAX);
        let pick = usize::try_from(state % bound).unwrap_or(0);
        rest.swap(last, pick);
    }
}

/// Puts `items[keep..]` back in the order `original` had them. Each original
/// entry claims one matching pending entry, in order; whatever matches none
/// — a track queued while the queue was shuffled — follows them, in the order
/// it is in now. Nothing is dropped and nothing is added.
pub fn restore_order(items: &mut Vec<PathBuf>, keep: usize, original: &[PathBuf]) {
    let keep = keep.min(items.len());
    let mut pending: Vec<Option<PathBuf>> = items.drain(keep..).map(Some).collect();
    for wanted in original {
        if let Some(slot) = pending
            .iter_mut()
            .find(|slot| slot.as_ref() == Some(wanted))
        {
            items.extend(slot.take());
        }
    }
    items.extend(pending.into_iter().flatten());
}

/// A seed that differs from one shuffle to the next.
#[must_use]
pub fn seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0x9E37_79B9_7F4A_7C15, |elapsed| {
            u64::try_from(elapsed.as_nanos() & u128::from(u64::MAX)).unwrap_or(1)
        })
}

#[cfg(test)]
mod tests {
    use super::{restore_order, shuffle_after};
    use std::path::PathBuf;

    #[test]
    fn a_shuffle_keeps_the_head_and_permutes_the_rest() {
        let mut items: Vec<u32> = (0..20).collect();
        shuffle_after(&mut items, 3, 42);

        assert_eq!(items[..3], [0, 1, 2], "played and playing stay put");
        let mut rest = items[3..].to_vec();
        rest.sort_unstable();
        assert_eq!(rest, (3..20).collect::<Vec<_>>(), "a permutation");
        assert_ne!(items[3..], (3..20).collect::<Vec<_>>()[..], "and mixed");
    }

    #[test]
    fn the_same_seed_mixes_the_same_way() {
        let mut a: Vec<u32> = (0..10).collect();
        let mut b = a.clone();
        shuffle_after(&mut a, 0, 7);
        shuffle_after(&mut b, 0, 7);
        assert_eq!(a, b);
    }

    #[test]
    fn restoring_follows_the_original_and_appends_what_it_never_had() {
        let p = |name: &str| PathBuf::from(name);
        let original = [p("a"), p("b"), p("c")];
        let mut items = vec![p("a"), p("c"), p("new"), p("b")];

        restore_order(&mut items, 1, &original);
        assert_eq!(items, [p("a"), p("b"), p("c"), p("new")]);
    }
}
