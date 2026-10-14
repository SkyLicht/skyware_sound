// SPDX-License-Identifier: GPL-3.0-or-later
//! Note names and the drum map.

/// Drum names and their General MIDI percussion notes.
const DRUMS: &[(&str, u8)] = &[
    ("bd", 36),
    ("kick", 36),
    ("rim", 37),
    ("rs", 37),
    ("sd", 38),
    ("sn", 38),
    ("snare", 38),
    ("cp", 39),
    ("clap", 39),
    ("lt", 41),
    ("hh", 42),
    ("ch", 42),
    ("mt", 45),
    ("tom", 45),
    ("oh", 46),
    ("ht", 48),
    ("cr", 49),
    ("crash", 49),
    ("rd", 51),
    ("ride", 51),
    ("tb", 54),
    ("tamb", 54),
    ("cb", 56),
    ("cowbell", 56),
    ("perc", 60),
    ("sh", 70),
    ("shaker", 70),
    ("cl", 75),
    ("claves", 75),
];

/// Maps common drum names to General MIDI drum notes, so `bd sd hh` can drive
/// a drum kit. Matching ignores ASCII case. Unknown names return `None`.
///
/// | Names | Note | GM sound |
/// |---|---|---|
/// | `bd`, `kick` | 36 | Bass Drum 1 |
/// | `rim`, `rs` | 37 | Side Stick |
/// | `sd`, `sn`, `snare` | 38 | Acoustic Snare |
/// | `cp`, `clap` | 39 | Hand Clap |
/// | `lt` | 41 | Low Floor Tom |
/// | `hh`, `ch` | 42 | Closed Hi-Hat |
/// | `mt`, `tom` | 45 | Low Tom |
/// | `oh` | 46 | Open Hi-Hat |
/// | `ht` | 48 | Hi-Mid Tom |
/// | `cr`, `crash` | 49 | Crash Cymbal 1 |
/// | `rd`, `ride` | 51 | Ride Cymbal 1 |
/// | `tb`, `tamb` | 54 | Tambourine |
/// | `cb`, `cowbell` | 56 | Cowbell |
/// | `perc` | 60 | Hi Bongo |
/// | `sh`, `shaker` | 70 | Maracas |
/// | `cl`, `claves` | 75 | Claves |
pub fn drum_note(name: &str) -> Option<u8> {
    DRUMS
        .iter()
        .find(|(drum, _)| drum.eq_ignore_ascii_case(name))
        .map(|&(_, note)| note)
}

/// Every drum name for General MIDI note `note`, such as `sd`, `sn` and
/// `snare` for 38. Empty for notes that have no drum name.
pub fn drum_names(note: u8) -> impl Iterator<Item = &'static str> {
    DRUMS
        .iter()
        .filter(move |&&(_, n)| n == note)
        .map(|&(name, _)| name)
}

/// Reads a note name such as `c4`, `F#3`, `Ds1` or `eb5` as a MIDI note
/// (`c4` is 60). Without an octave the note is in octave 3.
pub fn note_number(name: &str) -> Option<u8> {
    match note_name(name) {
        NoteName::Note(n) => Some(n),
        _ => None,
    }
}

/// Result of reading a word as a note name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoteName {
    /// The word isn't shaped like a note name.
    NotANote,
    /// A valid note.
    Note(u8),
    /// Shaped like a note name but outside MIDI's 0-127.
    OutOfRange,
}

/// Reads note names such as `c`, `c4`, `f#3`, `eb5` and `cs4`. The letter may be
/// upper case; the accidental (`#`, `s` sharp, `b` flat) is lower case. The
/// octave defaults to 3, and `c4` is 60.
pub(crate) fn note_name(word: &str) -> NoteName {
    let Some(&letter) = word.as_bytes().first() else {
        return NoteName::NotANote;
    };
    let base: i64 = match letter.to_ascii_lowercase() {
        b'c' => 0,
        b'd' => 2,
        b'e' => 4,
        b'f' => 5,
        b'g' => 7,
        b'a' => 9,
        b'b' => 11,
        _ => return NoteName::NotANote,
    };
    let mut rest = &word[1..];
    let accidental = match rest.as_bytes().first() {
        Some(b'#' | b's') => 1,
        Some(b'b') => -1,
        _ => 0,
    };
    if accidental != 0 {
        rest = &rest[1..];
    }
    let octave: i64 = if rest.is_empty() {
        3
    } else if rest.bytes().all(|b| b.is_ascii_digit()) {
        match rest.parse::<u32>() {
            Ok(octave) => i64::from(octave),
            Err(_) => return NoteName::OutOfRange,
        }
    } else {
        return NoteName::NotANote;
    };
    match u8::try_from((octave + 1) * 12 + base + accidental) {
        Ok(note) if note <= 127 => NoteName::Note(note),
        _ => NoteName::OutOfRange,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_names() {
        assert_eq!(note_name("c"), NoteName::Note(48));
        assert_eq!(note_name("c4"), NoteName::Note(60));
        assert_eq!(note_name("C4"), NoteName::Note(60));
        assert_eq!(note_name("a"), NoteName::Note(57));
        assert_eq!(note_name("a4"), NoteName::Note(69));
        assert_eq!(note_name("b"), NoteName::Note(59));
        assert_eq!(note_name("f#3"), NoteName::Note(54));
        assert_eq!(note_name("eb5"), NoteName::Note(75));
        assert_eq!(note_name("cs4"), NoteName::Note(61));
        assert_eq!(note_name("bb"), NoteName::Note(58));
        assert_eq!(note_name("c0"), NoteName::Note(12));
        assert_eq!(note_name("cb0"), NoteName::Note(11));
        assert_eq!(note_name("g9"), NoteName::Note(127));
        assert_eq!(note_name("g#9"), NoteName::OutOfRange);
        assert_eq!(note_name("c10"), NoteName::OutOfRange);
        assert_eq!(note_name("c99999999999"), NoteName::OutOfRange);
        for word in ["bd", "sd", "hh", "h", "c4x", "bass", "cbs", "", "e_"] {
            assert_eq!(note_name(word), NoteName::NotANote, "{word}");
        }
    }

    #[test]
    fn drums() {
        assert_eq!(drum_note("bd"), Some(36));
        assert_eq!(drum_note("kick"), Some(36));
        assert_eq!(drum_note("sd"), Some(38));
        assert_eq!(drum_note("rim"), Some(37));
        assert_eq!(drum_note("cp"), Some(39));
        assert_eq!(drum_note("hh"), Some(42));
        assert_eq!(drum_note("oh"), Some(46));
        assert_eq!(drum_note("lt"), Some(41));
        assert_eq!(drum_note("mt"), Some(45));
        assert_eq!(drum_note("tom"), Some(45));
        assert_eq!(drum_note("ht"), Some(48));
        assert_eq!(drum_note("crash"), Some(49));
        assert_eq!(drum_note("ride"), Some(51));
        assert_eq!(drum_note("cb"), Some(56));
        assert_eq!(drum_note("BD"), Some(36));
        assert_eq!(drum_note("piano"), None);
        assert_eq!(drum_note(""), None);
    }

    #[test]
    fn drum_aliases_and_note_numbers() {
        assert_eq!(drum_names(38).collect::<Vec<_>>(), ["sd", "sn", "snare"]);
        assert_eq!(drum_names(36).collect::<Vec<_>>(), ["bd", "kick"]);
        assert_eq!(drum_names(61).count(), 0);
        assert_eq!(note_number("A0"), Some(21));
        assert_eq!(note_number("Ds1"), Some(27));
        assert_eq!(note_number("A#3"), Some(58));
        assert_eq!(note_number("C4"), Some(60));
        assert_eq!(note_number("bd"), None);
        assert_eq!(note_number("g#9"), None);
    }

    #[test]
    fn only_cb_is_both_a_drum_and_a_note() {
        let both: Vec<&str> = DRUMS
            .iter()
            .map(|&(name, _)| name)
            .filter(|name| note_name(name) != NoteName::NotANote)
            .collect();
        assert_eq!(both, ["cb"]);
    }
}
