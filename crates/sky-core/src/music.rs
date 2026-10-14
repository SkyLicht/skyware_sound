// SPDX-License-Identifier: MIT OR Apache-2.0
//! Note names and note durations.

use crate::CoreError;
use crate::time::{PPQ, Ticks};

const NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// Parses a note name such as `C4`, `f#3`, `Bb-1` into a MIDI note number.
/// Middle C is `C4` = 60.
pub fn parse_note_name(text: &str) -> Result<u8, CoreError> {
    let err = || CoreError::InvalidNoteName(text.to_string());
    let s = text.trim();
    let mut chars = s.chars();
    let letter = chars.next().ok_or_else(err)?.to_ascii_uppercase();
    let base: i32 = match letter {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return Err(err()),
    };
    let rest = chars.as_str();
    let (accidental, octave_str) = match rest.chars().next() {
        Some('#') | Some('s') => (1, &rest[1..]),
        Some('b') => (-1, &rest[1..]),
        _ => (0, rest),
    };
    let octave: i32 = octave_str.parse().map_err(|_| err())?;
    let midi = (octave + 1) * 12 + base + accidental;
    u8::try_from(midi)
        .ok()
        .filter(|m| *m <= 127)
        .ok_or_else(err)
}

/// Formats a MIDI note number as a name with sharps, such as `C4` or `F#3`.
pub fn note_name(midi: u8) -> String {
    let octave = midi as i32 / 12 - 1;
    format!("{}{}", NAMES[(midi % 12) as usize], octave)
}

/// Parses a duration written as a fraction of a whole note: `1/4` is a
/// quarter note, `3/8` three eighths, `1` a whole note, `1/8.` a dotted
/// eighth and `1/8t` an eighth-note triplet. Returns ticks.
pub fn parse_duration(text: &str) -> Result<Ticks, CoreError> {
    let err = || CoreError::InvalidDuration(text.to_string());
    let mut s = text.trim();
    let mut factor_num = 1i64;
    let mut factor_den = 1i64;
    if let Some(stripped) = s.strip_suffix('.') {
        s = stripped;
        factor_num = 3;
        factor_den = 2;
    } else if let Some(stripped) = s.strip_suffix('t') {
        s = stripped;
        factor_num = 2;
        factor_den = 3;
    }
    let (num, den) = match s.split_once('/') {
        Some((n, d)) => (
            n.trim().parse::<i64>().map_err(|_| err())?,
            d.trim().parse::<i64>().map_err(|_| err())?,
        ),
        None => (s.parse::<i64>().map_err(|_| err())?, 1),
    };
    if num <= 0 || den <= 0 {
        return Err(err());
    }
    let whole = PPQ * 4;
    let ticks = whole * num * factor_num / (den * factor_den);
    if ticks <= 0 {
        return Err(err());
    }
    Ok(ticks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_names() {
        assert_eq!(parse_note_name("C4").unwrap(), 60);
        assert_eq!(parse_note_name("a4").unwrap(), 69);
        assert_eq!(parse_note_name("F#3").unwrap(), 54);
        assert_eq!(parse_note_name("Bb2").unwrap(), 46);
        assert_eq!(parse_note_name("C-1").unwrap(), 0);
        assert_eq!(parse_note_name("G9").unwrap(), 127);
        assert!(parse_note_name("G#9").is_err());
        assert!(parse_note_name("H2").is_err());
        assert_eq!(note_name(61), "C#4");
        for n in 0..=127u8 {
            assert_eq!(parse_note_name(&note_name(n)).unwrap(), n);
        }
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("1/4").unwrap(), 960);
        assert_eq!(parse_duration("1").unwrap(), 3840);
        assert_eq!(parse_duration("3/8").unwrap(), 1440);
        assert_eq!(parse_duration("1/8.").unwrap(), 720);
        assert_eq!(parse_duration("1/8t").unwrap(), 320);
        assert!(parse_duration("0/4").is_err());
        assert!(parse_duration("x").is_err());
    }
}
