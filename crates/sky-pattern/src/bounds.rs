// SPDX-License-Identifier: GPL-3.0-or-later
//! Static bounds that keep queries exact, overflow-free and cheap.
//!
//! A query walks the tree with 128-bit rationals. Its absolute bounds are
//! clamped to ±2^31 cycles and have denominators of at most 2^32. Going down
//! the tree, each speed factor `p/q` and each slot `[a/b, a/b + c/d)` changes
//! the numbers the walk computes in a predictable way, so every node records
//! worst-case products along its paths to the atoms:
//!
//! | Field | Grows by (factor `p/q`) | Grows by (slot) | Bounds |
//! |---|---|---|---|
//! | `den` | `p` | `lcm(b, d)` | denominators of event times |
//! | `slow` | `ceil(q/p)` | 1 | absolute length of one local cycle |
//! | `win` | `q` | `b * c` | growth of query-range denominators |
//! | `fast` | `ceil(p/q)` | 1 | growth of query-range magnitudes |
//!
//! An event time is at most 2^31 + `slow` in size with a denominator of at
//! most `den`, so it fits in an `i64` numerator when `den <= 2^30` and
//! `slow * den <= 2^61`. A query-range value is at most (2^31 + 1) * `fast` in
//! size with a denominator of at most 2^32 * `win`, so its numerator stays
//! below 2^125 when `fast * win <= 2^61`.
//!
//! `events` bounds how many events start in one cycle (sequences and stacks
//! add up their children), so a typo like `bd*9999*9999` is rejected rather
//! than freezing the host.

use crate::ast::Q;

/// Most events a pattern may start in one cycle.
pub(crate) const MAX_EVENTS_PER_CYCLE: u64 = 1 << 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Bounds {
    pub den: u64,
    pub slow: u64,
    pub win: u64,
    pub fast: u64,
    pub events: u64,
}

/// Why a pattern's bounds are too large.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Problem {
    /// Times would get too fine to represent exactly.
    TooFine,
    /// Too many events per cycle.
    TooDense,
}

/// The positive integer `|n|`, saturating to `u64::MAX`.
fn magnitude(n: i128) -> u64 {
    u64::try_from(n.unsigned_abs()).unwrap_or(u64::MAX)
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

impl Bounds {
    /// Bounds of an atom: one event per cycle and no change of scale.
    pub const ONE: Bounds = Bounds {
        den: 1,
        slow: 1,
        win: 1,
        fast: 1,
        events: 1,
    };

    /// Bounds of silence, and the starting point for combining children.
    pub const SILENT: Bounds = Bounds {
        events: 0,
        ..Bounds::ONE
    };

    /// What a speed factor adds. A window of one cycle covers `p/q` local
    /// cycles, touching at most `ceil(p/q)` of them, plus one when it isn't
    /// aligned.
    pub fn factor(factor: Q) -> Bounds {
        let (p, q) = (magnitude(*factor.numer()), magnitude(*factor.denom()));
        let whole = p.div_ceil(q);
        Bounds {
            den: p,
            slow: q.div_ceil(p),
            win: q,
            fast: whole,
            events: if q == 1 { whole } else { whole + 1 },
        }
    }

    /// What a slot of a sequence adds; the sequence adds up the slots' events.
    pub fn slot(start: Q, len: Q) -> Bounds {
        let b = magnitude(*start.denom());
        let d = magnitude(*len.denom());
        Bounds {
            den: (b / gcd(b, d)).saturating_mul(d),
            slow: 1,
            win: b.saturating_mul(magnitude(*len.numer())),
            fast: 1,
            events: 1,
        }
    }

    /// What a shift in time by `by` adds: its denominator, and one more
    /// cycle's worth of events when it isn't whole. Shifts are at most a
    /// few thousand cycles, so query magnitudes at most double.
    pub fn shift(by: Q) -> Bounds {
        let q = magnitude(*by.denom());
        Bounds {
            den: q,
            slow: 1,
            win: q,
            fast: 2,
            events: if q == 1 { 1 } else { 2 },
        }
    }

    /// What splitting each event into `n` adds.
    pub fn ply(n: Q) -> Bounds {
        let n = magnitude(*n.numer());
        Bounds {
            den: n,
            slow: 1,
            win: n,
            fast: 2,
            events: n,
        }
    }

    /// Bounds for events of `main` that look up values in `other`, each
    /// taking up to `layers` of them.
    pub fn lookup(main: Bounds, other: Bounds, layers: u64) -> Bounds {
        let both = main.max(other);
        Bounds {
            fast: both.fast.saturating_mul(2),
            events: main.events.saturating_mul(layers),
            ..both
        }
    }

    /// The bounds of `self` applied on top of `inner`.
    pub fn then(self, inner: Bounds) -> Bounds {
        Bounds {
            den: self.den.saturating_mul(inner.den),
            slow: self.slow.saturating_mul(inner.slow),
            win: self.win.saturating_mul(inner.win),
            fast: self.fast.saturating_mul(inner.fast),
            events: self.events.saturating_mul(inner.events),
        }
    }

    /// Bounds covering either `self` or `other`.
    pub fn max(self, other: Bounds) -> Bounds {
        Bounds {
            den: self.den.max(other.den),
            slow: self.slow.max(other.slow),
            win: self.win.max(other.win),
            fast: self.fast.max(other.fast),
            events: self.events.max(other.events),
        }
    }

    /// Bounds covering `self` and `other` playing together.
    pub fn both(self, other: Bounds) -> Bounds {
        Bounds {
            events: self.events.saturating_add(other.events),
            ..self.max(other)
        }
    }

    /// What, if anything, makes these bounds too large to query safely.
    pub fn problem(self) -> Option<Problem> {
        if self.den > 1 << 30
            || self.slow.saturating_mul(self.den) > 1 << 61
            || self.fast.saturating_mul(self.win) > 1 << 61
        {
            Some(Problem::TooFine)
        } else if self.events > MAX_EVENTS_PER_CYCLE {
            Some(Problem::TooDense)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factor_bounds() {
        let b = Bounds::factor(Q::new(3, 2));
        assert_eq!((b.den, b.slow, b.win, b.fast, b.events), (3, 1, 2, 2, 3));
        let b = Bounds::factor(Q::new(1, 4));
        assert_eq!((b.den, b.slow, b.win, b.fast, b.events), (1, 4, 4, 1, 2));
        let b = Bounds::factor(Q::new(4, 1));
        assert_eq!((b.fast, b.events), (4, 4));
    }

    #[test]
    fn slot_bounds() {
        // The second of three equal steps.
        let b = Bounds::slot(Q::new(1, 3), Q::new(1, 3));
        assert_eq!((b.den, b.win), (3, 3));
        // `a@3 b`: the slot [3/4, 1).
        let b = Bounds::slot(Q::new(3, 4), Q::new(1, 4));
        assert_eq!((b.den, b.win), (4, 4));
        // `a@1.5 b`: the slot [0, 3/5).
        let b = Bounds::slot(Q::new(0, 1), Q::new(3, 5));
        assert_eq!((b.den, b.win), (5, 3));
    }

    #[test]
    fn combining() {
        let a = Bounds::factor(Q::new(3, 2));
        let b = Bounds::factor(Q::new(5, 7));
        assert_eq!(a.then(b).den, 15);
        assert_eq!(a.max(b).win, 7);
        assert_eq!(a.both(b).events, 5);
        assert_eq!(Bounds::ONE.problem(), None);
        let fine = Bounds {
            den: 1 << 31,
            ..Bounds::ONE
        };
        assert_eq!(fine.problem(), Some(Problem::TooFine));
        let dense = Bounds {
            events: MAX_EVENTS_PER_CYCLE + 1,
            ..Bounds::ONE
        };
        assert_eq!(dense.problem(), Some(Problem::TooDense));
        let saturated = Bounds::factor(Q::new(i128::MAX, 1));
        assert!(saturated.then(saturated).problem().is_some());
    }
}
