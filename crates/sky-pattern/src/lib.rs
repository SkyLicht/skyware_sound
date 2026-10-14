// SPDX-License-Identifier: GPL-3.0-or-later
//! Rhythm and melody patterns written in a Tidal-style mini-notation.
//!
//! A pattern describes one *cycle* of music that repeats forever. The DAW
//! decides how long a cycle lasts (usually one bar). [`parse`] turns text into
//! a [`Pattern`], and [`Pattern::query`] lists the [`Event`]s whose onsets fall
//! inside a time range, with exact rational times measured in cycles.
//!
//! ```
//! use sky_pattern::{Time, Value, parse};
//!
//! let pattern = parse("bd*2, hh*4").unwrap();
//! let events = pattern.query(Time::from_integer(0), Time::from_integer(1), 0);
//! assert_eq!(events.len(), 6);
//! assert_eq!(events[0].value, Value::Sound { name: "bd".into(), index: 0 });
//! assert_eq!(events[0].value.midi_note(), Some(36));
//! assert_eq!(events[1].begin, Time::new(0, 1)); // the first hh, also at 0
//! assert_eq!(events[2].begin, Time::new(1, 4));
//! ```
//!
//! # Syntax
//!
//! | Syntax | Meaning | Example |
//! |---|---|---|
//! | `a b c` | Steps separated by whitespace share their span equally | `bd sd hh` |
//! | `~` | A rest: takes a step, plays nothing | `bd ~ sd ~` |
//! | `[ ]` | A subsequence that fits in one step | `bd [sd sd]` |
//! | `,` | Layers that play at the same time, each over the full span | `bd*2, hh*4`, `[c4,e4,g4]` |
//! | `< >` | Alternation: one step per cycle | `<c e g>`, `<[a b] c>` |
//! | `|` | Random choice between whole sequences, once per cycle | `bd sd \| hh hh hh` |
//! | `x*n` | Play `x` `n` times per step (`n` may be a decimal) | `hh*4`, `bd*1.5` |
//! | `x/n` | Stretch `x` over `n` steps | `bd/2` |
//! | `x!n`, `x!` | Repeat a step (`n` copies in total; bare `!` adds one more) | `bd!3 sd`, `bd ! sd` |
//! | `x@n` | Give a step weight `n` when dividing the span | `c@3 e` |
//! | `_` | Lengthen the previous step by one weight unit | `c _ _ e` |
//! | `x(k,n)`, `x(k,n,r)` | Euclidean rhythm: `k` onsets over `n` steps, rotated left by `r` | `bd(3,8)`, `bd(3,8,2)` |
//! | `x?`, `x?p` | Drop events with probability 0.5, or `p` | `hh*8?`, `hh*8?0.3` |
//! | `name:i` | Sample index `i` of a sound | `sd:3` |
//!
//! Modifiers stack and apply left to right: `bd*2?`, `[bd sd]*2`,
//! `<bd sd>(3,8)`, `hh*4@2`. Inside one bracket, `|` binds more loosely than
//! `,`, so `a, b | c` chooses between the stack `a, b` and `c`.
//!
//! ## Atoms
//!
//! * Note names `a`–`g` with an optional accidental (`#` or `s` for sharp, `b`
//!   for flat) and an optional octave: `c4` = 60, `f#3` = 54, `eb5` = 75,
//!   `cs4` = 61. Without an octave the note is in octave 3, so `c` = 48 and
//!   `a` = 57. Upper-case letters work too (`C4`).
//! * Whole numbers 0–127 are MIDI notes: `60 64 67`.
//! * Any other name (`[a-zA-Z][a-zA-Z0-9_]*`, optionally with `:index`) is a
//!   [`Value::Sound`]. Drum names known to [`drum_note`] always mean the
//!   drum, so `cb` is the cowbell rather than C flat (write `cb3` for that).
//!
//! ## Alternation and time
//!
//! `<a b c>` behaves like `[a b c]/3`: each element takes one cycle of the
//! time the alternation lives in. When the alternation is sped up it
//! advances once per repetition, so `<a b>*2` plays `a b` every cycle, and in
//! a sequence such as `<a b> c` it advances once per cycle. A nested
//! alternation keeps its own count: `<a <b c>>` plays `a b a c`.
//!
//! ## Randomness
//!
//! `?` and `|` never use global state. Each decision is a hash (splitmix64)
//! of the query `seed`, the source position of the operator, and the
//! absolute time of the event (for `?`) or of the cycle (for `|`), so a query
//! always returns the same events for the same seed, however the time range
//! is split up.
//!
//! ## Limits
//!
//! So that one typo can't freeze the host or overflow the exact arithmetic:
//!
//! * speed factors and weights are at most 10000, with at most four decimal
//!   places; `!` counts and Euclidean step counts are at most 1024;
//! * a pattern may start at most 65536 events per cycle (`bd*256*257` is an
//!   error);
//! * nesting that would make times too fine to keep exact is an error, such
//!   as `[[[bd*1.333]*1.333]*1.333]*1.333` (one or two levels of such
//!   decimals are fine);
//! * queries are clamped to ±2^31 cycles.

mod ast;
mod bounds;
mod lexer;
mod parser;
mod query;
mod value;

use std::sync::Arc;

use serde::{Deserialize, Serialize};

pub use value::{drum_names, drum_note, note_number};

/// Time measured in cycles. One cycle is one pattern repetition (the DAW maps a
/// cycle to e.g. one bar).
pub type Time = num_rational::Rational64;

/// What an event plays.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Value {
    /// A named sound such as `bd`, `sd:3` (index 3). Index defaults to 0.
    Sound { name: String, index: u32 },
    /// A MIDI note number, from a note name (`c4`, `f#3`, `eb5`, `c` = octave 3
    /// default, so `c` = 48, `c4` = 60) or a bare integer 0-127 (`60`).
    Note(u8),
}

impl Value {
    /// The MIDI note this value plays: `Note(n)` gives `n`; a sound gives its
    /// General MIDI drum note from [`drum_note`] (the index is ignored), or
    /// `None` for sounds that aren't drums.
    pub fn midi_note(&self) -> Option<u8> {
        match self {
            Value::Note(note) => Some(*note),
            Value::Sound { name, .. } => drum_note(name),
        }
    }
}

/// A byte range in the pattern's source text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

/// One sounding event of a pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    /// Onset in cycles.
    pub begin: Time,
    /// End in cycles; always after `begin`.
    pub end: Time,
    pub value: Value,
    /// Where in the source text this event's atom came from.
    pub source: Span,
}

/// Why a pattern failed to parse, and where.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message} at column {column}")]
pub struct ParseError {
    pub message: String,
    /// Byte offset of the offending text.
    pub offset: usize,
    /// Length of the offending text in bytes.
    pub len: usize,
    /// 1-based column, counted in characters from the start of the line.
    pub column: usize,
}

impl ParseError {
    pub(crate) fn new(src: &str, span: Span, message: impl Into<String>) -> Self {
        let line_start = src[..span.start].rfind('\n').map_or(0, |i| i + 1);
        ParseError {
            message: message.into(),
            offset: span.start,
            len: span.end - span.start,
            column: src[line_start..span.start].chars().count() + 1,
        }
    }
}

/// A parsed pattern. Cheap to clone; it can be shared between threads.
#[derive(Clone, Debug)]
pub struct Pattern {
    source: String,
    root: Arc<ast::Node>,
}

/// Parses mini-notation text. Empty text, or text with only rests, is a valid
/// pattern that plays nothing.
pub fn parse(src: &str) -> Result<Pattern, ParseError> {
    Ok(Pattern {
        source: src.to_string(),
        root: parser::parse(src)?,
    })
}

impl Pattern {
    /// The text this pattern was parsed from.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// All events whose onset `begin` lies in `[from, to)`, sorted by `begin`,
    /// then by `source.start`. Random choices depend only on `seed`, so the same
    /// query always gives the same answer.
    ///
    /// Bounds are clamped to ±2^31 cycles. A bound with a denominator above
    /// 2^32 is first rounded up to a multiple of 2^-32 cycles; the same bound
    /// always rounds the same way, so back-to-back queries still line up.
    pub fn query(&self, from: Time, to: Time, seed: u64) -> Vec<Event> {
        query::query(&self.root, from, to, seed)
    }

    /// Like [`Pattern::query`] with `f64` cycle bounds. Each bound is rounded
    /// down to a multiple of 2^-20 cycles, so back-to-back queries that share a
    /// bound never repeat or skip an event.
    pub fn query_cycles(&self, from: f64, to: f64, seed: u64) -> Vec<Event> {
        if from.is_nan() || to.is_nan() {
            return Vec::new();
        }
        let (from, to) = (query::bound_from_f64(from), query::bound_from_f64(to));
        query::query_bounds(&self.root, from, to, seed)
    }
}
