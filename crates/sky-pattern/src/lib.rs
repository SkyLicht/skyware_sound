// SPDX-License-Identifier: GPL-3.0-or-later
//! Temporary stub with the final API shape; replaced by the real pattern engine.

pub type Time = num_rational::Rational64;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Value {
    Sound { name: String, index: u32 },
    Note(u8),
}

impl Value {
    pub fn midi_note(&self) -> Option<u8> {
        match self {
            Value::Note(n) => Some(*n),
            Value::Sound { name, .. } => drum_note(name),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub begin: Time,
    pub end: Time,
    pub value: Value,
    pub source: Span,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message} at column {column}")]
pub struct ParseError {
    pub message: String,
    pub offset: usize,
    pub len: usize,
    pub column: usize,
}

#[derive(Clone, Debug)]
pub struct Pattern {
    source: String,
}

pub fn parse(src: &str) -> Result<Pattern, ParseError> {
    Ok(Pattern {
        source: src.to_string(),
    })
}

impl Pattern {
    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn query(&self, _from: Time, _to: Time, _seed: u64) -> Vec<Event> {
        Vec::new()
    }
}

pub fn drum_note(name: &str) -> Option<u8> {
    match name {
        "bd" => Some(36),
        "sd" => Some(38),
        "hh" => Some(42),
        _ => None,
    }
}
