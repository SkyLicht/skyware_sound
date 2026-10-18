// SPDX-License-Identifier: GPL-3.0-or-later
//! What Strudel's value functions do to one event: `.gain(0.5)`,
//! `.add(7)`, `.s("piano")`, `.bank("RolandTR909")`, `.scale("C:minor")`
//! and the like.

use std::sync::Arc;

use crate::ast::{Node, Q};
use crate::controls::{Control, Controls};
use crate::query::to_time;
use crate::signal::Signal;
use crate::{Event, Time, Value};

/// A change to each event of a pattern, by the value of an argument.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Op {
    /// Sets the control to the argument's number.
    Control(Control),
    /// Arithmetic on the pitch: a note, a number from `n`, or the `note`
    /// of a named sound.
    Add,
    Sub,
    Mul,
    Div,
    /// Plays the argument's sound (`.s("piano")`). A note becomes the
    /// sound's pitch and a number from `n` its sample index.
    Sound,
    /// Picks a sound's sample (`.n(2)`).
    Index,
    /// Sets the pitch (`.note("c3")`).
    Note,
    /// Puts the argument's name and `_` in front of the sound's name, so
    /// `bd` in bank `RolandTR909` is `RolandTR909_bd`.
    Bank,
    /// Turns scale degrees into notes; the argument names a scale in the
    /// table, such as `C:minor`.
    Scale(Arc<Vec<(String, Scale)>>),
    /// Multiplies the event's length (`.legato(0.5)`).
    Legato,
}

/// Where an [`Op`]'s argument comes from.
#[derive(Clone, Debug)]
pub(crate) enum Arg {
    Const(Value),
    /// A pattern, read at each event's start.
    Pattern(Arc<Node>),
    /// A signal, read at each event's start.
    Signal(Signal),
}

/// Largest factor `legato` can stretch an event by.
pub(crate) const MAX_LEGATO: i64 = 64;

fn to_q(t: Time) -> Q {
    Q::new(i128::from(*t.numer()), i128::from(*t.denom()))
}

fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(_) => v.number(),
        Value::Note(n) => Some(f64::from(*n)),
        Value::Sound { .. } => None,
    }
}

/// An exact number from a value: patterns give exact numbers, signals
/// rounded ones.
fn exact(v: &Value) -> Option<Q> {
    match v {
        Value::Number(t) => Some(to_q(*t)),
        Value::Note(n) => Some(Q::from_integer(i128::from(*n))),
        Value::Sound { .. } => None,
    }
}

/// A MIDI note from a number, or `None` outside 0-127.
fn midi(x: f64) -> Option<u8> {
    let n = x.round();
    (0.0..=127.0).contains(&n).then_some(n as u8)
}

fn arith(op: &Op, a: Q, b: Q) -> Option<Q> {
    use num_rational::Ratio;
    use num_traits::{CheckedAdd, CheckedDiv, CheckedMul, CheckedSub, Signed};
    let checked = |r: Option<Ratio<i128>>| r.filter(|r| r.numer().abs() < 1 << 62);
    match op {
        Op::Add => checked(a.checked_add(&b)),
        Op::Sub => checked(a.checked_sub(&b)),
        Op::Mul => checked(a.checked_mul(&b)),
        Op::Div if b != Q::from_integer(0) => checked(a.checked_div(&b)),
        _ => None,
    }
}

/// Changes `event` by `op` with the argument `arg`. False when the event
/// should be dropped, as when a note moves out of the MIDI range.
pub(crate) fn apply(op: &Op, event: &mut Event, arg: &Value) -> bool {
    let controls: &mut Controls = &mut event.controls;
    match op {
        Op::Control(control) => match number(arg) {
            Some(x) => {
                controls.set(*control, x);
                true
            }
            None => false,
        },
        Op::Add | Op::Sub | Op::Mul | Op::Div => {
            let Some(b) = exact(arg) else {
                return false;
            };
            match &mut event.value {
                Value::Note(n) => {
                    match arith(op, Q::from_integer(i128::from(*n)), b)
                        .and_then(|q| midi(f64_of(q)))
                    {
                        Some(n2) => {
                            *n = n2;
                            true
                        }
                        None => false,
                    }
                }
                Value::Number(t) => match arith(op, to_q(*t), b) {
                    Some(q) => {
                        *t = to_time(q);
                        true
                    }
                    None => false,
                },
                Value::Sound { .. } => {
                    if let Some(note) = controls.get(Control::Note) {
                        let Some(q) = arith(op, q_of(note), b) else {
                            return false;
                        };
                        controls.set(Control::Note, f64_of(q));
                    }
                    true
                }
            }
        }
        Op::Sound => {
            let Value::Sound {
                name,
                index: arg_index,
            } = arg
            else {
                return false;
            };
            let index = match &event.value {
                Value::Note(p) => {
                    controls.set(Control::Note, f64::from(*p));
                    0
                }
                Value::Number(_) => event
                    .value
                    .number()
                    .map_or(0, |n| n.round().clamp(0.0, f64::from(u32::MAX)) as u32),
                Value::Sound { index, .. } => *index,
            };
            event.value = Value::Sound {
                name: name.clone(),
                index: if *arg_index != 0 { *arg_index } else { index },
            };
            true
        }
        Op::Index => {
            let Some(x) = number(arg) else {
                return false;
            };
            match &mut event.value {
                Value::Sound { index, .. } => {
                    *index = x.round().clamp(0.0, f64::from(u32::MAX)) as u32;
                }
                Value::Number(t) => {
                    if let Some(q) = exact(arg) {
                        *t = to_time(q);
                    }
                }
                Value::Note(_) => {}
            }
            true
        }
        Op::Note => {
            let Some(x) = number(arg) else {
                return false;
            };
            match &mut event.value {
                Value::Sound { .. } => {
                    controls.set(Control::Note, x);
                    true
                }
                v => match midi(x) {
                    Some(n) => {
                        *v = Value::Note(n);
                        true
                    }
                    None => false,
                },
            }
        }
        Op::Bank => {
            let Value::Sound { name: bank, .. } = arg else {
                return false;
            };
            if let Value::Sound { name, .. } = &mut event.value {
                *name = format!("{bank}_{name}");
            }
            true
        }
        Op::Scale(table) => {
            let Value::Sound { name: scale, .. } = arg else {
                return false;
            };
            let Some((_, scale)) = table.iter().find(|(n, _)| n == scale) else {
                return false;
            };
            let degree = match &event.value {
                Value::Number(_) | Value::Note(_) => number(&event.value),
                Value::Sound { .. } => None,
            };
            match degree.and_then(|d| scale.note(d.round() as i64)) {
                Some(n) => {
                    event.value = Value::Note(n);
                    true
                }
                None => false,
            }
        }
        Op::Legato => {
            let Some(x) = exact(arg) else {
                return false;
            };
            if x <= Q::from_integer(0) || x > Q::from_integer(i128::from(MAX_LEGATO)) {
                return false;
            }
            let begin = to_q(event.begin);
            let end = begin + (to_q(event.end) - begin) * x;
            event.end = to_time(end);
            event.end > event.begin
        }
    }
}

pub(crate) fn f64_of(q: Q) -> f64 {
    *q.numer() as f64 / *q.denom() as f64
}

/// An `f64` as an exact rational, to six decimal places.
pub(crate) fn q_of(x: f64) -> Q {
    Q::new((x * 1e6).round() as i128, 1_000_000)
}

/// A scale: a root note and the steps above it in one octave.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Scale {
    root: i64,
    steps: &'static [i64],
}

/// Scale names, after the root, and their steps. Words are joined with
/// `:` or spaces, so `C:minor:pentatonic` and `C:minor pentatonic` agree.
const SCALES: &[(&str, &[i64])] = &[
    ("major", &[0, 2, 4, 5, 7, 9, 11]),
    ("ionian", &[0, 2, 4, 5, 7, 9, 11]),
    ("minor", &[0, 2, 3, 5, 7, 8, 10]),
    ("aeolian", &[0, 2, 3, 5, 7, 8, 10]),
    ("natural minor", &[0, 2, 3, 5, 7, 8, 10]),
    ("dorian", &[0, 2, 3, 5, 7, 9, 10]),
    ("phrygian", &[0, 1, 3, 5, 7, 8, 10]),
    ("lydian", &[0, 2, 4, 6, 7, 9, 11]),
    ("mixolydian", &[0, 2, 4, 5, 7, 9, 10]),
    ("locrian", &[0, 1, 3, 5, 6, 8, 10]),
    ("harmonic minor", &[0, 2, 3, 5, 7, 8, 11]),
    ("melodic minor", &[0, 2, 3, 5, 7, 9, 11]),
    ("harmonic major", &[0, 2, 4, 5, 7, 8, 11]),
    ("phrygian dominant", &[0, 1, 4, 5, 7, 8, 10]),
    ("major pentatonic", &[0, 2, 4, 7, 9]),
    ("pentatonic", &[0, 2, 4, 7, 9]),
    ("minor pentatonic", &[0, 3, 5, 7, 10]),
    ("blues", &[0, 3, 5, 6, 7, 10]),
    ("minor blues", &[0, 3, 5, 6, 7, 10]),
    ("major blues", &[0, 2, 3, 4, 7, 9]),
    ("whole tone", &[0, 2, 4, 6, 8, 10]),
    ("wholetone", &[0, 2, 4, 6, 8, 10]),
    ("diminished", &[0, 2, 3, 5, 6, 8, 9, 11]),
    ("chromatic", &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]),
    ("hirajoshi", &[0, 2, 3, 7, 8]),
    ("in-sen", &[0, 1, 5, 7, 10]),
    ("egyptian", &[0, 2, 5, 7, 10]),
];

impl Scale {
    /// Reads a scale name such as `C:major`, `A2:minor:pentatonic` or
    /// `F#4:dorian`. The root's octave defaults to 3, and a bare root is
    /// its major scale.
    pub fn parse(name: &str) -> Result<Scale, String> {
        let (root, kind) = match name.split_once(':') {
            Some((root, kind)) => (root, kind.replace(':', " ")),
            None => (name, "major".to_string()),
        };
        let root = match crate::value::note_name(root) {
            crate::value::NoteName::Note(n) => i64::from(n),
            _ => {
                return Err(format!(
                    "'{root}' isn't a root note; a scale is written like C:major or A2:minor"
                ));
            }
        };
        let kind = kind.trim().to_ascii_lowercase();
        match SCALES.iter().find(|(n, _)| *n == kind) {
            Some((_, steps)) => Ok(Scale { root, steps }),
            None => {
                let names: Vec<&str> = SCALES.iter().map(|(n, _)| *n).collect();
                Err(format!(
                    "unknown scale '{kind}'; the scales are {}",
                    names.join(", ")
                ))
            }
        }
    }

    /// Degree `d` (0 is the root; 7 an octave up in a seven-note scale).
    pub fn note(&self, d: i64) -> Option<u8> {
        let len = self.steps.len() as i64;
        let note = self.root + 12 * d.div_euclid(len) + self.steps[d.rem_euclid(len) as usize];
        u8::try_from(note).ok().filter(|n| *n <= 127)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales() {
        let s = Scale::parse("C:major").unwrap();
        let notes: Vec<u8> = (0..8).filter_map(|d| s.note(d)).collect();
        assert_eq!(notes, [48, 50, 52, 53, 55, 57, 59, 60]);
        assert_eq!(s.note(-1), Some(47));
        let s = Scale::parse("A2:minor:pentatonic").unwrap();
        assert_eq!(s.note(0), Some(45));
        assert_eq!(s.note(5), Some(57));
        assert_eq!(Scale::parse("F#4").unwrap().note(0), Some(66));
        assert!(Scale::parse("H:major").is_err());
        assert!(Scale::parse("C:nonsense").unwrap_err().contains("dorian"));
    }
}
