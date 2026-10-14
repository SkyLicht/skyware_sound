// SPDX-License-Identifier: MIT OR Apache-2.0
//! Picking the next variation of an SFX container.

use sky_core::manifest::Selection;

use crate::rng::Rng;

/// Per-asset selection state that persists between plays.
#[derive(Clone, Debug)]
pub(crate) struct Selector {
    mode: Selection,
    last: Option<usize>,
    next_in_order: usize,
    /// The current shuffle permutation, allocated once at load time.
    order: Box<[usize]>,
    /// The next index into `order`; `order.len()` means "reshuffle first".
    order_pos: usize,
}

impl Selector {
    pub(crate) fn new(mode: Selection, count: usize) -> Self {
        Self {
            mode,
            last: None,
            next_in_order: 0,
            order: (0..count).collect(),
            order_pos: count,
        }
    }

    /// Forgets all history, as if the asset had just been loaded.
    pub(crate) fn reset(&mut self) {
        self.last = None;
        self.next_in_order = 0;
        for (i, slot) in self.order.iter_mut().enumerate() {
            *slot = i;
        }
        self.order_pos = self.order.len();
    }

    /// The index of the variation to play next. `count` must be at least 1.
    pub(crate) fn next(&mut self, rng: &mut Rng) -> usize {
        let count = self.order.len();
        if count <= 1 {
            self.last = Some(0);
            return 0;
        }
        let pick = match self.mode {
            Selection::Random => rng.below(count),
            Selection::RandomNoRepeat => match self.last {
                Some(last) => {
                    // Pick among the other count - 1 variations.
                    let r = rng.below(count - 1);
                    if r >= last { r + 1 } else { r }
                }
                None => rng.below(count),
            },
            Selection::RoundRobin => {
                let pick = self.next_in_order % count;
                self.next_in_order = (pick + 1) % count;
                pick
            }
            Selection::Shuffle => {
                if self.order_pos >= count {
                    self.reshuffle(rng);
                }
                let pick = self.order[self.order_pos];
                self.order_pos += 1;
                pick
            }
        };
        self.last = Some(pick);
        pick
    }

    fn reshuffle(&mut self, rng: &mut Rng) {
        let count = self.order.len();
        for i in (1..count).rev() {
            let j = rng.below(i + 1);
            self.order.swap(i, j);
        }
        // Never play the same variation twice across the boundary.
        if let Some(last) = self.last
            && self.order[0] == last
        {
            let k = 1 + rng.below(count - 1);
            self.order.swap(0, k);
        }
        self.order_pos = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picks(mode: Selection, count: usize, n: usize) -> Vec<usize> {
        let mut rng = Rng::new(99);
        let mut selector = Selector::new(mode, count);
        (0..n).map(|_| selector.next(&mut rng)).collect()
    }

    #[test]
    fn single_variation_always_wins() {
        for mode in [
            Selection::Random,
            Selection::RandomNoRepeat,
            Selection::RoundRobin,
            Selection::Shuffle,
        ] {
            assert!(picks(mode, 1, 20).iter().all(|&p| p == 0));
        }
    }

    #[test]
    fn shuffle_with_two_alternates() {
        let p = picks(Selection::Shuffle, 2, 50);
        assert!(p.windows(2).all(|w| w[0] != w[1]), "{p:?}");
    }

    #[test]
    fn reset_restarts_the_sequence() {
        let mut rng = Rng::new(5);
        let mut s = Selector::new(Selection::RoundRobin, 3);
        s.next(&mut rng);
        s.next(&mut rng);
        s.reset();
        assert_eq!(s.next(&mut rng), 0);
    }
}
