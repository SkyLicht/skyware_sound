// SPDX-License-Identifier: GPL-3.0-or-later
//! The compact note list: a short text form for melodies and chords that is
//! cheaper and less error-prone for an AI to write than JSON objects.
//!
//! Entries are separated by commas or new lines and play one after another:
//!
//! ```text
//! C4 1/4 v90, E4 1/8, r 1/8, C4+E4+G4 1/2 v80, 67 1/4 @8
//! ```
//!
//! * a pitch is a note name (`C4`, `F#3`, `Bb2`) or a MIDI number (`60`),
//!   several joined with `+` make a chord, and `r` or `~` is a rest;
//! * the duration is a fraction of a whole note: `1/4`, `3/8`, `1/8.`
//!   (dotted) or `1/8t` (triplet);
//! * `v<1-127>` sets the velocity (default 100);
//! * `@<beats>` places the entry at that many quarter notes from the clip
//!   start; later entries continue from there.

use sky_core::music::{note_name, parse_duration, parse_note_name};
use sky_core::{PPQ, Ticks};

use crate::error::ModelError;
use crate::project::Note;

fn parse_pitch(text: &str) -> Result<u8, String> {
    if let Ok(n) = text.parse::<u8>() {
        if n <= 127 {
            return Ok(n);
        }
        return Err(format!("MIDI note {n} is above 127"));
    }
    parse_note_name(text).map_err(|e| e.to_string())
}

/// Parses a compact note list into notes, with start times relative to the clip.
pub fn parse_note_list(text: &str) -> Result<Vec<Note>, ModelError> {
    let mut notes = Vec::new();
    let mut cursor: Ticks = 0;
    for (i, raw) in text.split([',', '\n', ';']).enumerate() {
        let entry = raw.trim();
        if entry.is_empty() {
            continue;
        }
        let err = |m: String| ModelError::NoteList(format!("entry {} ({entry:?}): {m}", i + 1));
        let mut parts = entry.split_whitespace();
        let pitch_part = parts.next().ok_or_else(|| err("empty entry".into()))?;
        let dur_part = parts
            .next()
            .ok_or_else(|| err("missing a duration such as 1/4".into()))?;
        let length = parse_duration(dur_part).map_err(|e| err(e.to_string()))?;
        let mut velocity = 100u8;
        for extra in parts {
            if let Some(v) = extra.strip_prefix(['v', 'V']) {
                velocity = v
                    .parse::<u8>()
                    .ok()
                    .filter(|v| (1..=127).contains(v))
                    .ok_or_else(|| err(format!("velocity {v:?} must be 1-127")))?;
            } else if let Some(pos) = extra.strip_prefix('@') {
                let beats: f64 = pos
                    .parse()
                    .ok()
                    .filter(|b: &f64| b.is_finite() && *b >= 0.0)
                    .ok_or_else(|| {
                        err(format!("position {pos:?} must be a number of beats >= 0"))
                    })?;
                cursor = (beats * PPQ as f64).round() as Ticks;
            } else {
                return Err(err(format!(
                    "unexpected {extra:?}; use v<velocity> or @<beats>"
                )));
            }
        }
        if !matches!(pitch_part, "r" | "R" | "~" | "rest") {
            for p in pitch_part.split('+') {
                let pitch = parse_pitch(p).map_err(err)?;
                notes.push(Note::new(cursor, length, pitch, velocity));
            }
        }
        cursor += length;
    }
    Ok(notes)
}

fn format_duration(ticks: Ticks) -> String {
    let whole = PPQ * 4;
    let g = gcd(ticks, whole);
    let (n, d) = (ticks / g, whole / g);
    if d == 1 {
        n.to_string()
    } else {
        format!("{n}/{d}")
    }
}

fn gcd(a: i64, b: i64) -> i64 {
    if b == 0 {
        a.abs().max(1)
    } else {
        gcd(b, a % b)
    }
}

fn format_beats(ticks: Ticks) -> String {
    let beats = ticks as f64 / PPQ as f64;
    let s = format!("{beats:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Formats notes as a compact note list that parses back to the same notes
/// (apart from IDs). Chords are notes sharing start, length and velocity.
pub fn format_note_list(notes: &[Note]) -> String {
    let mut sorted: Vec<&Note> = notes.iter().collect();
    sorted.sort_by_key(|n| (n.start, n.length, n.velocity, n.pitch));
    let mut entries = Vec::new();
    let mut cursor: Ticks = 0;
    let mut i = 0;
    while i < sorted.len() {
        let first = sorted[i];
        let mut j = i + 1;
        while j < sorted.len()
            && sorted[j].start == first.start
            && sorted[j].length == first.length
            && sorted[j].velocity == first.velocity
        {
            j += 1;
        }
        let pitches: Vec<String> = sorted[i..j].iter().map(|n| note_name(n.pitch)).collect();
        let mut entry = format!("{} {}", pitches.join("+"), format_duration(first.length));
        if first.velocity != 100 {
            entry.push_str(&format!(" v{}", first.velocity));
        }
        if first.start != cursor {
            entry.push_str(&format!(" @{}", format_beats(first.start)));
        }
        entries.push(entry);
        cursor = first.start + first.length;
        i = j;
    }
    entries.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_melody_rests_and_chords() {
        let notes = parse_note_list("C4 1/4 v90, E4 1/8, r 1/8, C4+E4+G4 1/2 v80").unwrap();
        assert_eq!(notes.len(), 5);
        assert_eq!(
            (
                notes[0].start,
                notes[0].length,
                notes[0].pitch,
                notes[0].velocity
            ),
            (0, 960, 60, 90)
        );
        assert_eq!(
            (notes[1].start, notes[1].pitch, notes[1].velocity),
            (960, 64, 100)
        );
        assert!(
            notes[2..]
                .iter()
                .all(|n| n.start == 1920 && n.length == 1920)
        );
    }

    #[test]
    fn explicit_positions() {
        let notes = parse_note_list("60 1/4 @4, 62 1/4").unwrap();
        assert_eq!(notes[0].start, 3840);
        assert_eq!(notes[1].start, 4800);
    }

    #[test]
    fn errors_point_at_the_entry() {
        let e = parse_note_list("C4 1/4, H4 1/4").unwrap_err().to_string();
        assert!(e.contains("entry 2"), "{e}");
        assert!(parse_note_list("C4").is_err());
        assert!(parse_note_list("C4 1/4 v0").is_err());
        assert!(parse_note_list("C4 1/4 x").is_err());
    }

    #[test]
    fn format_round_trips() {
        let text = "C4 1/4 v90, E4 1/8, C4+E4+G4 1/2 v80 @2, A3 3/8 @6";
        let notes = parse_note_list(text).unwrap();
        let formatted = format_note_list(&notes);
        assert_eq!(formatted, text);
        let again = parse_note_list(&formatted).unwrap();
        let key = |n: &Note| (n.start, n.length, n.pitch, n.velocity);
        let mut a: Vec<_> = notes.iter().map(key).collect();
        let mut b: Vec<_> = again.iter().map(key).collect();
        a.sort();
        b.sort();
        assert_eq!(a, b);
    }
}
