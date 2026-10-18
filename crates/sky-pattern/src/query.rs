// SPDX-License-Identifier: GPL-3.0-or-later
//! Turns a pattern tree into the events of a time range.
//!
//! The walk carries the query range in each node's local time, plus the
//! affine map from local time back to absolute time. Atoms emit events in
//! absolute time directly, and random decisions hash absolute times, so the
//! results never depend on how a range is split into queries.

use std::cmp::{max, min};

use crate::ast::{Kind, MAX_LAYERS, Node, Q};
use crate::controls::Controls;
use crate::ops::{Arg, apply, f64_of, q_of};
use crate::{Event, Time, Value};

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

    fn to_local(self, t: Q) -> Q {
        (t - self.offset) / self.scale
    }
}

/// An event time inside the walk's arithmetic.
fn q(t: Time) -> Q {
    Q::new_raw(i128::from(*t.numer()), i128::from(*t.denom()))
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
pub(crate) fn to_time(q: Q) -> Time {
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
        Kind::Atom {
            value,
            span,
            controls,
        } => {
            let mut cycle = from.ceil();
            while cycle < to {
                out.push(Event {
                    begin: to_time(frame.to_absolute(cycle)),
                    end: to_time(frame.to_absolute(cycle + ONE)),
                    value: value.clone(),
                    source: *span,
                    controls: *controls,
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
        Kind::Degrade {
            salt,
            prob,
            invert,
            node,
        } => {
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
                if (unit(h) >= *prob) != *invert {
                    out.swap(kept, i);
                    kept += 1;
                }
            }
            out.truncate(kept);
        }
        Kind::Shift { by, node } => {
            let child_frame = Frame {
                scale: frame.scale,
                offset: frame.offset + by * frame.scale,
            };
            walk(node, from - by, to - by, child_frame, seed, out);
        }
        Kind::Rev(node) => {
            for c in cycles(from, to) {
                let cycle = Q::from_integer(c);
                let (a, b) = (frame.to_absolute(cycle), frame.to_absolute(cycle + ONE));
                let lo = frame.to_absolute(max(from, cycle));
                let hi = frame.to_absolute(min(to, cycle + ONE));
                for mut e in collect(node, cycle, cycle + ONE, frame, seed) {
                    let begin = a + b - min(q(e.end), b);
                    let end = a + b - q(e.begin);
                    if lo <= begin && begin < hi && begin < end {
                        e.begin = to_time(begin);
                        e.end = to_time(end);
                        out.push(e);
                    }
                }
            }
        }
        Kind::Iter { n, back, node } => {
            for c in cycles(from, to) {
                let cycle = Q::from_integer(c);
                let start = max(from, cycle);
                let end = min(to, cycle + ONE);
                if start >= end {
                    continue;
                }
                let step = Q::new(c.rem_euclid(*n), *n);
                let by = if *back { -step } else { step };
                // Child time runs `by` ahead of this node's.
                let child_frame = Frame {
                    scale: frame.scale,
                    offset: frame.offset - by * frame.scale,
                };
                walk(node, start + by, end + by, child_frame, seed, out);
            }
        }
        Kind::Cycles(nodes) => {
            for c in cycles(from, to) {
                let cycle = Q::from_integer(c);
                let start = max(from, cycle);
                let end = min(to, cycle + ONE);
                if start < end {
                    let pick = c.rem_euclid(nodes.len() as i128) as usize;
                    walk(&nodes[pick], start, end, frame, seed, out);
                }
            }
        }
        Kind::CycleChance {
            salt,
            prob,
            yes,
            no,
        } => {
            for c in cycles(from, to) {
                let cycle = Q::from_integer(c);
                let start = max(from, cycle);
                let end = min(to, cycle + ONE);
                if start >= end {
                    continue;
                }
                let [a, b, c, d] = words(frame.to_absolute(cycle));
                let pick = if unit(hash(seed, &[*salt, a, b, c, d])) < *prob {
                    yes
                } else {
                    no
                };
                walk(pick, start, end, frame, seed, out);
            }
        }
        Kind::Ply { n, node } => {
            let (lo, hi) = (frame.to_absolute(from), frame.to_absolute(to));
            for e in collect(node, from - lookback(node), to, frame, seed) {
                let begin = q(e.begin);
                let len = (q(e.end) - begin) / *n;
                for i in 0..*n {
                    let b = begin + len * i;
                    if lo <= b && b < hi {
                        out.push(Event {
                            begin: to_time(b),
                            end: to_time(b + len),
                            ..e.clone()
                        });
                    }
                }
            }
        }
        Kind::Struct { mask, node } => {
            let marks = collect(mask, from, to, frame, seed);
            if marks.is_empty() {
                return;
            }
            let values = sorted(collect(node, from - lookback(node), to, frame, seed));
            for m in marks {
                for v in sounding(&values, m.begin) {
                    out.push(Event {
                        begin: m.begin,
                        end: m.end,
                        ..v.clone()
                    });
                }
            }
        }
        Kind::Mask { mask, node } => {
            let first = out.len();
            walk(node, from, to, frame, seed, out);
            if out.len() == first {
                return;
            }
            let marks = sorted(collect(mask, from - lookback(mask), to, frame, seed));
            let mut kept = first;
            for i in first..out.len() {
                if sounding(&marks, out[i].begin).next().is_some() {
                    out.swap(kept, i);
                    kept += 1;
                }
            }
            out.truncate(kept);
        }
        Kind::Set { node, op, arg } => {
            let first = out.len();
            walk(node, from, to, frame, seed, out);
            if out.len() == first {
                return;
            }
            let events: Vec<Event> = out.drain(first..).collect();
            match arg {
                Arg::Const(value) => {
                    for mut e in events {
                        if apply(op, &mut e, value) {
                            out.push(e);
                        }
                    }
                }
                Arg::Signal(signal) => {
                    for mut e in events {
                        let t = f64_of(frame.to_local(q(e.begin)));
                        let value = Value::Number(to_time(q_of(signal.at(t, seed))));
                        if apply(op, &mut e, &value) {
                            out.push(e);
                        }
                    }
                }
                Arg::Pattern(arg) => {
                    let values = sorted(collect(arg, from - lookback(arg), to, frame, seed));
                    for e in events {
                        for v in sounding(&values, e.begin) {
                            let mut e = e.clone();
                            if apply(op, &mut e, &v.value) {
                                out.push(e);
                            }
                        }
                    }
                }
            }
        }
        Kind::Switch { arg, cases } => {
            for a in collect(arg, from - lookback(arg), to, frame, seed) {
                let start = max(from, frame.to_local(q(a.begin)));
                let end = min(to, frame.to_local(q(a.end)));
                if start >= end {
                    continue;
                }
                if let Some((_, case)) = cases.iter().find(|(v, _)| *v == a.value) {
                    walk(case, start, end, frame, seed, out);
                }
            }
        }
        Kind::Segment { n, signal, span } => {
            let steps = Q::from_integer(*n);
            let mut k = (from * steps).ceil();
            while k < to * steps {
                let t = k / steps;
                let value = signal.at(f64_of(t), seed);
                out.push(Event {
                    begin: to_time(frame.to_absolute(t)),
                    end: to_time(frame.to_absolute(t + steps.recip())),
                    value: Value::Number(to_time(q_of(value))),
                    source: *span,
                    controls: Controls::default(),
                });
                k += ONE;
            }
        }
        Kind::Swing { n, by, node } => {
            // As Strudel's swingBy: the second half of each slice starts
            // `by / 2` of a slice late.
            let delay = by / (*n * 2);
            let zero = Q::from_integer(0);
            let (lo, hi) = (from - delay.max(zero), to - delay.min(zero));
            for mut e in collect(node, lo, hi, frame, seed) {
                let begin = frame.to_local(q(e.begin));
                let slice = begin * *n;
                if slice - slice.floor() >= Q::new(1, 2) {
                    let (b, en) = (begin + delay, frame.to_local(q(e.end)) + delay);
                    e.begin = to_time(frame.to_absolute(b));
                    e.end = to_time(frame.to_absolute(en));
                    if from <= b && b < to {
                        out.push(e);
                    }
                } else if from <= begin && begin < to {
                    out.push(e);
                }
            }
        }
    }
}

/// The events of `node` with local onsets in `[from, to)`, unsorted.
fn collect(node: &Node, from: Q, to: Q, frame: Frame, seed: u64) -> Vec<Event> {
    let mut out = Vec::new();
    if from < to {
        walk(node, from, to, frame, seed, &mut out);
    }
    out
}

/// How far back, in a node's local cycles, an event can start and still
/// sound now.
fn lookback(node: &Node) -> Q {
    Q::from_integer(i128::from(node.bounds.slow))
}

fn sorted(mut events: Vec<Event>) -> Vec<Event> {
    events.sort_by_key(|a| (a.begin, a.source.start));
    events
}

/// The events of `events` (sorted by onset) sounding at time `t`, at most
/// [`MAX_LAYERS`] of them.
fn sounding(events: &[Event], t: Time) -> impl Iterator<Item = &Event> {
    let upto = events.partition_point(|e| e.begin <= t);
    events[..upto]
        .iter()
        .filter(move |e| e.end > t)
        .take(MAX_LAYERS as usize)
}

fn splitmix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

pub(crate) fn hash(seed: u64, parts: &[u64]) -> u64 {
    parts
        .iter()
        .fold(splitmix64(seed), |h, &part| splitmix64(h ^ part))
}

/// A hash mapped to `[0, 1)`.
pub(crate) fn unit(h: u64) -> f64 {
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
