// SPDX-License-Identifier: GPL-3.0-or-later
//! Turns a pattern tree into the events of a time range.
//!
//! The walk carries the query range in each node's local time, plus the
//! affine map from local time back to absolute time. Atoms emit events in
//! absolute time directly, and random decisions hash absolute times, so the
//! results never depend on how a range is split into queries.

use std::cmp::{max, min};

use crate::ast::{Kind, Node, Q};
use crate::{Event, Time};

/// Queries are clamped to ±2^31 cycles (about 136 years of 2-second bars).
const LIMIT: i128 = 1 << 31;
/// Query bounds with larger denominators are rounded up to a multiple of
/// 1/`FINE`, which keeps the walk's arithmetic inside 128 bits.
const FINE: i128 = 1 << 32;
/// `f64` bounds are rounded down to a multiple of 1/`COARSE`.
const COARSE: i128 = 1 << 20;

const ONE: Q = Q::new_raw(1, 1);

/// Maps a node's local time `t` to absolute time `t * scale + offset`.
#[derive(Clone, Copy)]
struct Frame {
    scale: Q,
    offset: Q,
}

impl Frame {
    fn to_absolute(self, t: Q) -> Q {
        t * self.scale + self.offset
    }
}

/// A query bound as used by the walk.
fn bound(t: Time) -> Q {
    let q = Q::new_raw(i128::from(*t.numer()), i128::from(*t.denom()));
    let q = q.clamp(Q::from_integer(-LIMIT), Q::from_integer(LIMIT));
    if *q.denom() > FINE {
        (q * FINE).ceil() / FINE
    } else {
        q
    }
}

/// Converts cycles to a bound, rounding down to a multiple of 2^-20.
pub(crate) fn bound_from_f64(cycles: f64) -> Q {
    let limit = LIMIT as f64;
    let scaled = (cycles.clamp(-limit, limit) * COARSE as f64).floor();
    Q::new(scaled as i128, COARSE)
}

/// Converts an event time back to the public type. The complexity limit
/// guarantees it fits; the fallback only avoids a panic.
fn to_time(q: Q) -> Time {
    match (i64::try_from(*q.numer()), i64::try_from(*q.denom())) {
        (Ok(numer), Ok(denom)) => Time::new_raw(numer, denom),
        _ => {
            let scaled = (q * COARSE).floor().to_integer();
            Time::new(i64::try_from(scaled).unwrap_or(i64::MAX), COARSE as i64)
        }
    }
}

pub(crate) fn query(root: &Node, from: Time, to: Time, seed: u64) -> Vec<Event> {
    query_bounds(root, bound(from), bound(to), seed)
}

pub(crate) fn query_bounds(root: &Node, from: Q, to: Q, seed: u64) -> Vec<Event> {
    let mut out = Vec::new();
    if from < to {
        let frame = Frame {
            scale: ONE,
            offset: Q::new_raw(0, 1),
        };
        walk(root, from, to, frame, seed, &mut out);
    }
    out.sort_by(|a, b| {
        (a.begin, a.source.start, a.source.end, a.end).cmp(&(
            b.begin,
            b.source.start,
            b.source.end,
            b.end,
        ))
    });
    out
}

/// Integer cycles `c` whose span `[c, c + 1)` overlaps `[from, to)`.
fn cycles(from: Q, to: Q) -> std::ops::Range<i128> {
    from.floor().to_integer()..to.ceil().to_integer()
}

/// Splits a rational into words for hashing.
fn words(q: Q) -> [u64; 4] {
    let (n, d) = (q.numer().cast_unsigned(), q.denom().cast_unsigned());
    [n as u64, (n >> 64) as u64, d as u64, (d >> 64) as u64]
}

/// Appends the events of `node` with local onsets in `[from, to)`; `from < to`.
fn walk(node: &Node, from: Q, to: Q, frame: Frame, seed: u64, out: &mut Vec<Event>) {
    match &node.kind {
        Kind::Silence => {}
        Kind::Atom { value, span } => {
            let mut cycle = from.ceil();
            while cycle < to {
                out.push(Event {
                    begin: to_time(frame.to_absolute(cycle)),
                    end: to_time(frame.to_absolute(cycle + ONE)),
                    value: value.clone(),
                    source: *span,
                });
                cycle += ONE;
            }
        }
        Kind::Seq(slots) => {
            for c in cycles(from, to) {
                let cycle = Q::from_integer(c);
                // The query range relative to the start of this cycle.
                let lo = from - cycle;
                let hi = to - cycle;
                let first = slots.partition_point(|slot| slot.end <= lo);
                for slot in slots[first..].iter().take_while(|slot| slot.start < hi) {
                    // Inside the slot, child time runs from `cycle` to `cycle + 1`.
                    let child_from = cycle + (max(lo, slot.start) - slot.start) / slot.len;
                    let child_to = cycle + (min(hi, slot.end) - slot.start) / slot.len;
                    let child_frame = Frame {
                        scale: frame.scale * slot.len,
                        offset: frame.to_absolute(cycle + slot.start - cycle * slot.len),
                    };
                    walk(&slot.node, child_from, child_to, child_frame, seed, out);
                }
            }
        }
        Kind::Stack(layers) => {
            for layer in layers {
                walk(layer, from, to, frame, seed, out);
            }
        }
        Kind::Fast { factor, node } => {
            let child_frame = Frame {
                scale: frame.scale / factor,
                offset: frame.offset,
            };
            walk(node, from * factor, to * factor, child_frame, seed, out);
        }
        Kind::Choice { salt, alts } => {
            for c in cycles(from, to) {
                let cycle = Q::from_integer(c);
                let start = max(from, cycle);
                let end = min(to, cycle + ONE);
                if start >= end {
                    continue;
                }
                let [a, b, c, d] = words(frame.to_absolute(cycle));
                let h = hash(seed, &[*salt, a, b, c, d]);
                let pick = (h % alts.len() as u64) as usize;
                walk(&alts[pick], start, end, frame, seed, out);
            }
        }
        Kind::Degrade { salt, prob, node } => {
            let first = out.len();
            walk(node, from, to, frame, seed, out);
            let mut kept = first;
            for i in first..out.len() {
                let event = &out[i];
                let h = hash(
                    seed,
                    &[
                        *salt,
                        event.source.start as u64,
                        event.begin.numer().cast_unsigned(),
                        event.begin.denom().cast_unsigned(),
                    ],
                );
                if unit(h) >= *prob {
                    out.swap(kept, i);
                    kept += 1;
                }
            }
            out.truncate(kept);
        }
    }
}

fn splitmix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn hash(seed: u64, parts: &[u64]) -> u64 {
    parts
        .iter()
        .fold(splitmix64(seed), |h, &part| splitmix64(h ^ part))
}

/// A hash mapped to `[0, 1)`.
fn unit(h: u64) -> f64 {
    (h >> 11) as f64 / (1u64 << 53) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f64_bounds_round_down() {
        assert_eq!(bound_from_f64(0.0), Q::from_integer(0));
        assert_eq!(bound_from_f64(2.5), Q::new(5, 2));
        assert_eq!(bound_from_f64(-0.25), Q::new(-1, 4));
        assert!(bound_from_f64(1.0 / 3.0) < Q::new(1, 3));
        assert!(bound_from_f64(2.0 / 3.0) < Q::new(2, 3));
        assert!(bound_from_f64(0.2) < Q::new(1, 5));
        assert_eq!(bound_from_f64(f64::INFINITY), Q::from_integer(LIMIT));
        assert_eq!(bound_from_f64(f64::NEG_INFINITY), Q::from_integer(-LIMIT));
    }

    #[test]
    fn rational_bounds() {
        assert_eq!(bound(Time::new(7, 3)), Q::new(7, 3));
        assert_eq!(bound(Time::from_integer(i64::MAX)), Q::from_integer(LIMIT));
        assert_eq!(bound(Time::from_integer(i64::MIN)), Q::from_integer(-LIMIT));
        // A huge denominator is rounded up onto the 2^-32 grid.
        let fine = bound(Time::new(1, (1 << 40) + 1));
        assert_eq!(fine, Q::new(1, FINE));
    }

    #[test]
    fn time_conversion() {
        assert_eq!(to_time(Q::new(-7, 3)), Time::new(-7, 3));
        let huge = Q::new(1, i128::from(i64::MAX) * 4);
        assert_eq!(to_time(huge), Time::from_integer(0));
    }

    #[test]
    fn unit_is_in_range() {
        assert_eq!(unit(0), 0.0);
        assert!(unit(u64::MAX) < 1.0);
    }

    #[test]
    fn hash_depends_on_every_part() {
        let base = hash(1, &[2, 3]);
        assert_ne!(base, hash(0, &[2, 3]));
        assert_ne!(base, hash(1, &[3, 2]));
        assert_ne!(base, hash(1, &[2, 4]));
        assert_eq!(base, hash(1, &[2, 3]));
    }
}
