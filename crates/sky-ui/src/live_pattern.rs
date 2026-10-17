// SPDX-License-Identifier: GPL-3.0-or-later
//! The live side of the pattern editor: colored tokens, the steps that are
//! sounding, completion of the track's sound names, and a step grid that
//! writes mini-notation for people who would rather click than type.

use std::ops::Range;

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Stroke};
use sky_core::Ticks;
use sky_dsp::InstrumentType;
use sky_model::{Clip, Track};
use sky_pattern::{Event, Pattern, Span, Time, Value};

use crate::theme;

/// What a piece of pattern text is, for its color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A named sound such as `bd` or `sd:3`.
    Sound,
    /// A note name such as `c4` or `f#3`.
    Note,
    Number,
    /// `~` and `_`.
    Rest,
    /// `[ ] < > ( )`.
    Bracket,
    /// `* / ! @ ? : -`.
    Operator,
    /// `,` and `|`.
    Separator,
    Space,
    /// A character the notation has no use for.
    Unknown,
}

/// Splits pattern text into colored pieces, as byte ranges. Unlike the
/// parser it never fails, so text being typed still gets its colors.
pub fn tokens(src: &str) -> Vec<(Kind, Range<usize>)> {
    let bytes = src.as_bytes();
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'#';
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        let ch = src[i..].chars().next().unwrap_or_default();
        let kind = match ch {
            'a'..='z' | 'A'..='Z' => {
                i += 1;
                while i < bytes.len() && word(bytes[i]) {
                    i += 1;
                }
                if bytes.get(i) == Some(&b':') && bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
                    i += 1;
                    while i < bytes.len() && bytes[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                if sky_pattern::note_number(&src[start..i]).is_some() {
                    Kind::Note
                } else {
                    Kind::Sound
                }
            }
            '0'..='9' | '.' => {
                while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                    i += 1;
                }
                Kind::Number
            }
            '~' | '_' => Kind::Rest,
            '[' | ']' | '<' | '>' | '(' | ')' => Kind::Bracket,
            '*' | '/' | '!' | '@' | '?' | ':' | '-' => Kind::Operator,
            ',' | '|' => Kind::Separator,
            c if c.is_whitespace() => {
                while let Some(c) = src[i..].chars().next().filter(|c| c.is_whitespace()) {
                    i += c.len_utf8();
                }
                Kind::Space
            }
            _ => Kind::Unknown,
        };
        if i == start {
            i += ch.len_utf8().max(1);
        }
        out.push((kind, start..i));
    }
    out
}

fn color(kind: Kind) -> Color32 {
    match kind {
        Kind::Sound => theme::lighten(theme::ACCENT, 0.35),
        Kind::Note => theme::GAME_LOOP,
        Kind::Number => theme::WARN,
        Kind::Rest => theme::TEXT_DIM,
        Kind::Bracket | Kind::Separator | Kind::Space => theme::TEXT_WEAK,
        Kind::Operator => Color32::from_rgb(196, 146, 255),
        Kind::Unknown => theme::ERROR,
    }
}

/// The color of `samples(...)` calls in pattern text.
const CODE: Color32 = Color32::from_rgb(110, 200, 190);

/// The pattern text with each piece in its color, the atoms in `lit` (the
/// steps sounding now) lit up, and `error` underlined. `code` is where the
/// text's `samples(...)` calls are, which are not mini-notation.
pub fn layout(
    src: &str,
    font: &FontId,
    lit: &[Span],
    error: Option<Range<usize>>,
    code: &[Range<usize>],
) -> LayoutJob {
    let mut job = LayoutJob::default();
    // The calls read as spaces, so the notation around them keeps its colors.
    let mut plain = src.as_bytes().to_vec();
    for r in code {
        plain[r.clone()].fill(b' ');
    }
    let plain = String::from_utf8_lossy(&plain);
    let mut pieces = Vec::new();
    for (kind, r) in tokens(&plain) {
        let mut at = r.start;
        for c in code.iter().filter(|c| c.start < r.end && r.start < c.end) {
            if at < c.start {
                pieces.push((Some(kind), at..c.start));
            }
            pieces.push((None, c.start.max(at)..c.end.min(r.end)));
            at = c.end.min(r.end);
        }
        if at < r.end {
            pieces.push((Some(kind), at..r.end));
        }
    }
    for (kind, r) in pieces {
        if r.is_empty() || !src.is_char_boundary(r.start) || !src.is_char_boundary(r.end) {
            continue;
        }
        let overlaps = |a: usize, b: usize| a < r.end && r.start < b;
        let mut format = TextFormat::simple(font.clone(), kind.map_or(CODE, color));
        if kind.is_some_and(|k| matches!(k, Kind::Sound | Kind::Note | Kind::Number))
            && lit.iter().any(|s| overlaps(s.start, s.end))
        {
            format.background = theme::ACCENT;
            format.color = Color32::BLACK;
        }
        if let Some(e) = &error
            && (overlaps(e.start, e.end) || (e.is_empty() && r.start == e.start))
        {
            format.underline = Stroke::new(2.0, theme::ERROR);
            format.color = theme::ERROR;
        }
        job.append(&src[r], 0.0, format);
    }
    job
}

/// How far back to look for notes still sounding, in cycles.
const LOOKBACK: f64 = 4.0;

/// Where the playhead is in a pattern clip, in cycles from the clip's
/// start; `None` outside the clip.
pub fn cycle_at(clip: &Clip, cycle: Ticks, playhead: f64) -> Option<f64> {
    let t = playhead - clip.start as f64;
    (cycle > 0 && (0.0..clip.length as f64).contains(&t)).then(|| t / cycle as f64)
}

pub fn cycles(t: Time) -> f64 {
    *t.numer() as f64 / *t.denom() as f64
}

/// The events sounding `at` cycles in.
pub fn sounding(pattern: &Pattern, at: f64, seed: u64) -> Vec<Event> {
    let from = (at.floor() - LOOKBACK).max(0.0);
    pattern
        .query_cycles(from, at.floor() + 1.0, seed)
        .into_iter()
        .filter(|e| cycles(e.begin) <= at && at < cycles(e.end))
        .collect()
}

/// At most this many names are offered at once.
pub const MAX_COMPLETIONS: usize = 8;

/// The names a pattern on this track can play: a bank's sounds, or the
/// drum names for the drum kit. Other instruments play notes.
pub fn sound_names(track: &Track) -> Vec<String> {
    let Some(slot) = &track.instrument else {
        return Vec::new();
    };
    match slot.kind {
        InstrumentType::Bank => slot
            .bank
            .as_ref()
            .map(|b| b.sounds.keys().cloned().collect())
            .unwrap_or_default(),
        InstrumentType::Drums => (0..=127u8)
            .flat_map(sky_pattern::drum_names)
            .map(String::from)
            .collect(),
        _ => Vec::new(),
    }
}

/// The names a new grid row can take: one drum name for each drum piece,
/// a bank's sounds, or notes from C2 to C5 for the other instruments.
pub fn row_names(track: &Track) -> Vec<String> {
    match track.instrument.as_ref().map(|s| s.kind) {
        Some(InstrumentType::Drums) => (0..=127u8)
            .filter_map(|n| sky_pattern::drum_names(n).next())
            .map(String::from)
            .collect(),
        Some(InstrumentType::Bank) => sound_names(track),
        _ => (36..=72u8)
            .map(|n| sky_core::music::note_name(n).to_lowercase())
            .collect(),
    }
}

/// The word being typed just before the caret: where it starts and its
/// text. Both the caret and the start count characters.
pub fn word_before(text: &str, caret: usize) -> Option<(usize, String)> {
    let chars: Vec<char> = text.chars().collect();
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '#';
    // Not in the middle of a word.
    if caret > chars.len() || chars.get(caret).is_some_and(|c| is_word(*c) || *c == ':') {
        return None;
    }
    let mut start = caret;
    while start > 0 && is_word(chars[start - 1]) {
        start -= 1;
    }
    let word: String = chars[start..caret].iter().collect();
    word.starts_with(|c: char| c.is_ascii_alphabetic())
        .then_some((start, word))
}

/// The names that go on from `prefix`, shortest first.
pub fn completions(prefix: &str, names: &[String]) -> Vec<String> {
    let mut out: Vec<String> = names
        .iter()
        .filter(|n| {
            n.len() > prefix.len()
                && n.get(..prefix.len())
                    .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
        })
        .cloned()
        .collect();
    out.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    out.dedup();
    out.truncate(MAX_COMPLETIONS);
    out
}

/// Puts `name` in place of characters `start..caret` of `text`, and returns
/// the caret after it.
pub fn complete(text: &mut String, start: usize, caret: usize, name: &str) -> usize {
    let byte = |text: &str, i: usize| text.char_indices().nth(i).map_or(text.len(), |(b, _)| b);
    let (from, to) = (byte(text, start), byte(text, caret));
    text.replace_range(from..to, name);
    start + name.chars().count()
}

/// A pattern as rows of steps over one cycle: one row for each sound or
/// note, with the steps its hits start on.
#[derive(Clone, Debug, PartialEq)]
pub struct Grid {
    pub steps: usize,
    pub rows: Vec<Row>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// The sound or note as the pattern writes it, such as `bd` or `c4`.
    pub name: String,
    /// For each step, how many steps the hit starting there lasts.
    pub hits: Vec<Option<usize>>,
}

/// The step counts a grid can have.
pub const RESOLUTIONS: [usize; 5] = [8, 12, 16, 24, 32];

/// Which step counts to try first when none is asked for.
const PREFERRED: [usize; 5] = [16, 32, 12, 24, 8];

/// Cycle 0's events as (begin, end, value), in a fixed order.
fn first_cycle(pattern: &Pattern, seed: u64) -> Vec<(Time, Time, Value)> {
    let mut v: Vec<_> = pattern
        .query(Time::from_integer(0), Time::from_integer(1), seed)
        .into_iter()
        .map(|e| (e.begin, e.end, e.value))
        .collect();
    sort_events(&mut v);
    v
}

fn sort_events(v: &mut [(Time, Time, Value)]) {
    v.sort_by_cached_key(|(begin, end, value)| (*begin, *end, format!("{value:?}")));
}

/// Whether every event starts and ends on one of `steps` steps.
fn fits(events: &[Event], steps: usize) -> bool {
    let on_step = |t: Time| (t * Time::from_integer(steps as i64)).is_integer();
    events.iter().all(|e| on_step(e.begin) && on_step(e.end))
}

/// The pattern's grid at `steps` steps, or the first step count that fits.
/// Patterns that change from cycle to cycle, or whose notes fall between
/// steps or overlap, have none, and the reason is given instead.
pub fn grid_of(text: &str, seed: u64, steps: Option<usize>) -> Result<Grid, &'static str> {
    if sky_pattern::is_code(text) {
        return Err("The grid edits mini-notation; this pattern is Strudel code");
    }
    let pattern = sky_pattern::parse(text).map_err(|_| "Fix the pattern to see its grid")?;
    let events = pattern.query(Time::from_integer(0), Time::from_integer(1), seed);
    let first = first_cycle(&pattern, seed);
    for c in 1..4 {
        let shift = Time::from_integer(c);
        let mut later: Vec<_> = pattern
            .query(shift, shift + 1, seed)
            .into_iter()
            .map(|e| (e.begin - shift, e.end - shift, e.value))
            .collect();
        sort_events(&mut later);
        if later != first {
            return Err("This pattern changes from cycle to cycle, so one grid can't show it");
        }
    }
    if events.iter().any(|e| e.end > Time::from_integer(1)) {
        return Err("Some notes last past the end of the cycle");
    }
    let steps = match steps.filter(|s| fits(&events, *s)) {
        Some(s) => s,
        None => *PREFERRED
            .iter()
            .find(|s| fits(&events, **s))
            .ok_or("Some notes fall between the grid's steps")?,
    };
    // One row for each value, in the order the text first names them.
    let mut order: Vec<&Event> = events.iter().collect();
    order.sort_by_key(|e| e.source.start);
    let mut rows: Vec<(Value, Row)> = Vec::new();
    for e in order {
        if !rows.iter().any(|(v, _)| *v == e.value) {
            let name = text[e.source.start..e.source.end].to_string();
            rows.push((
                e.value.clone(),
                Row {
                    name,
                    hits: vec![None; steps],
                },
            ));
        }
    }
    let at = |t: Time| (t * Time::from_integer(steps as i64)).to_integer() as usize;
    let mut sorted: Vec<&Event> = events.iter().collect();
    sorted.sort_by_key(|e| e.begin);
    for e in sorted {
        let (_, row) = rows
            .iter_mut()
            .find(|(v, _)| *v == e.value)
            .expect("a row for each value");
        let (start, end) = (at(e.begin), at(e.end));
        let busy = row.hits[..start]
            .iter()
            .enumerate()
            .any(|(i, h)| h.is_some_and(|len| i + len > start));
        if row.hits[start].is_some() || busy {
            return Err("Some hits of one sound overlap");
        }
        row.hits[start] = Some(end - start);
    }
    let grid = Grid {
        steps,
        rows: rows.into_iter().map(|(_, r)| r).collect(),
    };
    // What the grid writes must play the same as the text.
    let written = sky_pattern::parse(&grid_text(&grid))
        .map_err(|_| "This pattern can't be written as a grid")?;
    if first_cycle(&written, seed) != first {
        return Err("This pattern can't be written as a grid");
    }
    Ok(grid)
}

/// Whether a grid at `steps` steps can show the pattern.
pub fn grid_fits(text: &str, seed: u64, steps: usize) -> bool {
    grid_of(text, seed, Some(steps)).is_ok_and(|g| g.steps == steps)
}

/// One row as mini-notation, in as few steps as it needs.
fn row_text(row: &Row) -> Option<String> {
    let steps = row.hits.len();
    if row.hits.iter().all(Option::is_none) {
        return None;
    }
    // The coarsest step that every hit starts and ends on.
    let unit = (1..=steps)
        .rev()
        .filter(|d| steps.is_multiple_of(*d))
        .find(|d| {
            row.hits
                .iter()
                .enumerate()
                .all(|(i, h)| h.is_none_or(|len| i % d == 0 && len % d == 0))
        })
        .unwrap_or(1);
    let slots = steps / unit;
    let every = (0..slots).all(|s| row.hits[s * unit] == Some(unit));
    if every {
        return Some(if slots == 1 {
            row.name.clone()
        } else {
            format!("{}*{slots}", row.name)
        });
    }
    let mut words = Vec::with_capacity(slots);
    let mut s = 0;
    while s < slots {
        match row.hits[s * unit] {
            Some(len) => {
                let held = (len / unit).clamp(1, slots - s);
                words.push(row.name.clone());
                words.extend(std::iter::repeat_n("_".to_string(), held - 1));
                s += held;
            }
            None => {
                words.push("~".to_string());
                s += 1;
            }
        }
    }
    Some(words.join(" "))
}

/// The grid as mini-notation: one layer for each row, a line each.
pub fn grid_text(grid: &Grid) -> String {
    grid.rows
        .iter()
        .filter_map(row_text)
        .collect::<Vec<_>>()
        .join(",\n")
}

impl Row {
    /// Turns the step on or off. A step inside a longer hit splits it, the
    /// new hit taking the rest of its length.
    pub fn toggle(&mut self, step: usize) {
        if step >= self.hits.len() {
            return;
        }
        if self.hits[step].take().is_some() {
            return;
        }
        let covering = (0..step).rev().find_map(|i| {
            self.hits[i]
                .filter(|len| i + len > step)
                .map(|len| (i, len))
        });
        let len = match covering {
            Some((i, len)) => {
                self.hits[i] = Some(step - i);
                i + len - step
            }
            None => 1,
        };
        self.hits[step] = Some(len);
    }

    /// Whether a hit is sounding on this step, and whether it starts there.
    pub fn at(&self, step: usize) -> (bool, bool) {
        if self.hits.get(step).is_some_and(Option::is_some) {
            return (true, true);
        }
        let held = (0..step)
            .rev()
            .find_map(|i| self.hits[i].map(|len| i + len > step))
            .unwrap_or(false);
        (held, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<(Kind, &str)> {
        tokens(src)
            .into_iter()
            .filter(|(k, _)| *k != Kind::Space)
            .map(|(k, r)| (k, &src[r]))
            .collect()
    }

    #[test]
    fn samples_calls_keep_their_text_in_their_own_color() {
        let src = "samples('github:a/b') // é\nbd sd";
        let code: Vec<Range<usize>> = sky_library::find_samples(src)
            .unwrap()
            .into_iter()
            .map(|c| c.range)
            .collect();
        assert_eq!(code, vec![0..21]);
        let job = layout(src, &FontId::monospace(14.0), &[], None, &code);
        assert_eq!(job.text, src);
        let colors: Vec<(&str, Color32)> = job
            .sections
            .iter()
            .map(|s| {
                (
                    &src[s.byte_range.start.0..s.byte_range.end.0],
                    s.format.color,
                )
            })
            .collect();
        assert_eq!(colors[0], ("samples('github:a/b')", CODE));
        assert!(colors.contains(&("bd", color(Kind::Sound))), "{colors:?}");
    }

    #[test]
    fn strudel_code_is_colored_apart_from_its_strings() {
        let src = r#"s("bd*2 sd").fast(2)"#;
        let code = sky_pattern::code_ranges(src);
        let job = layout(src, &FontId::monospace(14.0), &[], None, &code);
        assert_eq!(job.text, src);
        let colors: Vec<(&str, Color32)> = job
            .sections
            .iter()
            .map(|s| {
                (
                    &src[s.byte_range.start.0..s.byte_range.end.0],
                    s.format.color,
                )
            })
            .collect();
        assert_eq!(colors[0], ("s(\"", CODE));
        assert!(colors.contains(&("bd", color(Kind::Sound))), "{colors:?}");
        assert!(colors.contains(&("\").fast(2)", CODE)), "{colors:?}");
        // The grid only edits mini-notation.
        assert!(grid_of(src, 0, None).unwrap_err().contains("Strudel code"));
        assert!(grid_of("bd*2 sd", 0, None).is_ok());
    }

    #[test]
    fn tokens_are_classed_for_their_colors() {
        assert_eq!(
            kinds("bd*2 [sd:3 ~], <c4 f#3>(3,8) hh?\n60 _ {"),
            [
                (Kind::Sound, "bd"),
                (Kind::Operator, "*"),
                (Kind::Number, "2"),
                (Kind::Bracket, "["),
                (Kind::Sound, "sd:3"),
                (Kind::Rest, "~"),
                (Kind::Bracket, "]"),
                (Kind::Separator, ","),
                (Kind::Bracket, "<"),
                (Kind::Note, "c4"),
                (Kind::Note, "f#3"),
                (Kind::Bracket, ">"),
                (Kind::Bracket, "("),
                (Kind::Number, "3"),
                (Kind::Separator, ","),
                (Kind::Number, "8"),
                (Kind::Bracket, ")"),
                (Kind::Sound, "hh"),
                (Kind::Operator, "?"),
                (Kind::Number, "60"),
                (Kind::Rest, "_"),
                (Kind::Unknown, "{"),
            ]
        );
        // Every byte is in exactly one piece, multi-byte characters too.
        let src = "bd é ~";
        let joined: String = tokens(src).into_iter().map(|(_, r)| &src[r]).collect();
        assert_eq!(joined, src);
    }

    #[test]
    fn the_sounding_steps_follow_the_playhead() {
        let p = sky_pattern::parse("bd sd, hh*4").unwrap();
        let names = |at: f64| -> Vec<String> {
            let mut v: Vec<String> = sounding(&p, at, 0)
                .iter()
                .map(|e| p.source()[e.source.start..e.source.end].to_string())
                .collect();
            v.sort();
            v
        };
        assert_eq!(names(0.1), ["bd", "hh"]);
        assert_eq!(names(0.6), ["hh", "sd"]);
        assert_eq!(names(3.6), ["hh", "sd"]);
        // A note that lasts two cycles is still sounding in the second.
        let slow = sky_pattern::parse("bd/2").unwrap();
        assert_eq!(sounding(&slow, 1.5, 0).len(), 1);
        let clip = Clip {
            id: sky_core::ClipId::new(),
            name: "p".into(),
            start: 960,
            length: 1920,
            muted: false,
            content: sky_model::ClipContent::Midi { notes: vec![] },
        };
        assert_eq!(cycle_at(&clip, 960, 1440.0), Some(0.5));
        assert_eq!(cycle_at(&clip, 960, 900.0), None);
        assert_eq!(cycle_at(&clip, 960, 2880.0), None);
    }

    #[test]
    fn names_complete_the_word_before_the_caret() {
        let names: Vec<String> = ["hh", "hh27", "ht", "bd", "HighTom"]
            .map(String::from)
            .to_vec();
        assert_eq!(word_before("bd h", 4), Some((3, "h".to_string())));
        assert_eq!(word_before("bd h", 3), None);
        assert_eq!(word_before("bd hh", 4), None, "inside a word");
        assert_eq!(word_before("sd:3", 4), None);
        assert_eq!(word_before("é h", 3), Some((2, "h".to_string())));
        assert_eq!(completions("h", &names), ["hh", "ht", "hh27", "HighTom"]);
        assert_eq!(completions("hh", &names), ["hh27"]);
        assert!(completions("x", &names).is_empty());
        let mut text = "é bd h ~".to_string();
        assert_eq!(complete(&mut text, 5, 6, "hh27"), 9);
        assert_eq!(text, "é bd hh27 ~");
    }

    fn row(name: &str, hits: &[(usize, usize)], steps: usize) -> Row {
        let mut h = vec![None; steps];
        for (i, len) in hits {
            h[*i] = Some(*len);
        }
        Row {
            name: name.into(),
            hits: h,
        }
    }

    #[test]
    fn patterns_become_grids_and_back() {
        let g = grid_of("bd*4, ~ sd ~ sd, hh*8", 0, None).unwrap();
        assert_eq!(g.steps, 16);
        assert_eq!(g.rows.len(), 3);
        assert_eq!(g.rows[0], row("bd", &[(0, 4), (4, 4), (8, 4), (12, 4)], 16));
        assert_eq!(g.rows[1], row("sd", &[(4, 4), (12, 4)], 16));
        assert_eq!(grid_text(&g), "bd*4,\n~ sd ~ sd,\nhh*8");

        // Held notes keep their length, each note on its own row.
        let g = grid_of("c3 _ e3 ~", 0, None).unwrap();
        assert_eq!(grid_text(&g), "c3 ~,\n~ ~ e3 ~");
        let g = grid_of("c3 _ _ c3", 0, None).unwrap();
        assert_eq!(grid_text(&g), "c3 _ _ c3");
        let g = grid_of("bd ~ ~ bd ~ ~ bd ~", 0, Some(8)).unwrap();
        assert_eq!(g.steps, 8);
        assert_eq!(grid_text(&g), "bd ~ ~ bd ~ ~ bd ~");

        for (text, why) in [
            ("<bd sd>", "changes"),
            ("hh*16?", "changes"),
            ("bd@2 sd@3", "between"),
            ("bd, bd*2", "overlap"),
            ("bd [", "Fix"),
        ] {
            let e = grid_of(text, 0, None).unwrap_err();
            assert!(e.contains(why), "{text}: {e}");
        }
        assert!(grid_fits("bd*4", 0, 8));
        assert!(!grid_fits("hh*16", 0, 8));
        assert!(grid_of("", 0, None).unwrap().rows.is_empty());
    }

    #[test]
    fn toggling_steps_splits_and_clears_hits() {
        let mut r = row("c3", &[(0, 4)], 8);
        r.toggle(2);
        assert_eq!(r, row("c3", &[(0, 2), (2, 2)], 8));
        r.toggle(0);
        assert_eq!(r, row("c3", &[(2, 2)], 8));
        r.toggle(6);
        assert_eq!(r, row("c3", &[(2, 2), (6, 1)], 8));
        assert_eq!(r.at(3), (true, false));
        assert_eq!(r.at(4), (false, false));
        assert_eq!(r.at(6), (true, true));
        let g = Grid {
            steps: 8,
            rows: vec![r, row("bd", &[], 8)],
        };
        assert_eq!(grid_text(&g), "~ ~ c3 _ ~ ~ c3 ~");
    }
}
