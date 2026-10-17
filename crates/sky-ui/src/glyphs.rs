// SPDX-License-Identifier: GPL-3.0-or-later
//! The symbols pictures show, drawn as white lines on a 64-unit square.
//! They were drawn for Skyware Sound and are free to use (CC0).

use std::collections::HashMap;
use std::sync::OnceLock;

use egui::epaint::{CornerRadius, RectShape, StrokeKind};
use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, Vec2, pos2, vec2};
use sky_model::Symbol;

/// One shape of a symbol, in units of a 64-unit square.
#[derive(Clone, Copy)]
enum Form {
    /// SVG path data with absolute M, L, H, V, C, Q and Z.
    Path(&'static str),
    Circle(f32, f32, f32),
    Ellipse(f32, f32, f32, f32),
    /// x, y, width, height and corner radius.
    Rect(f32, f32, f32, f32, f32),
}

#[derive(Clone, Copy)]
struct Part {
    form: Form,
    /// Filled instead of stroked.
    fill: bool,
    /// Filled dark, for holes.
    dark: bool,
    /// Line width in units; 0 uses the symbol's normal width.
    width: f32,
    alpha: f32,
    /// Degrees turned around the middle of the square.
    turn: f32,
}

const fn part(form: Form) -> Part {
    Part {
        form,
        fill: false,
        dark: false,
        width: 0.0,
        alpha: 1.0,
        turn: 0.0,
    }
}

const fn path(d: &'static str) -> Part {
    part(Form::Path(d))
}

const fn circle(x: f32, y: f32, r: f32) -> Part {
    part(Form::Circle(x, y, r))
}

const fn ellipse(x: f32, y: f32, rx: f32, ry: f32) -> Part {
    part(Form::Ellipse(x, y, rx, ry))
}

const fn rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Part {
    part(Form::Rect(x, y, w, h, r))
}

impl Part {
    const fn w(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    const fn a(mut self, alpha: f32) -> Self {
        self.alpha = alpha;
        self
    }

    const fn filled(mut self) -> Self {
        self.fill = true;
        self
    }

    const fn hole(mut self) -> Self {
        self.fill = true;
        self.dark = true;
        self
    }

    const fn turned(mut self, degrees: f32) -> Self {
        self.turn = degrees;
        self
    }
}

const SYNTH: &[Part] = &[
    path("M8 25 L20 10 V25 L32 10 V25 L44 10 V25 L56 10"),
    rect(8.0, 33.0, 48.0, 20.0, 2.0),
    path("M14.9 33 V53 M21.7 33 V53 M28.6 33 V53 M35.4 33 V53 M42.3 33 V53 M49.1 33 V53").w(1.6),
    rect(13.2, 33.0, 3.4, 11.0, 0.6).filled(),
    rect(20.0, 33.0, 3.4, 11.0, 0.6).filled(),
    rect(33.7, 33.0, 3.4, 11.0, 0.6).filled(),
    rect(40.6, 33.0, 3.4, 11.0, 0.6).filled(),
    rect(47.4, 33.0, 3.4, 11.0, 0.6).filled(),
];

const ACID_BASS: &[Part] = &[
    circle(46.0, 17.0, 9.5),
    path("M46 17 L50.5 9.5").w(2.6),
    circle(17.0, 17.0, 6.0).w(2.0).a(0.7),
    path("M17 17 L13 12.5").w(2.0).a(0.7),
    path(
        "M5 47 C11 47 12.5 29 17 47 C19 54 21 47 25 47 C29 47 30 23 34.5 47 \
         C36.5 55 38.5 47 43 47 C47 47 49 35 52 47 L59 47",
    ),
];

const DRUMS: &[Part] = &[
    ellipse(32.0, 34.0, 21.0, 7.5),
    path("M11 34 V47 C11 56 53 56 53 47 V34"),
    path("M11 41 L18 49.5 L25 42 L32 50.5 L39 42 L46 49.5 L53 41")
        .w(1.5)
        .a(0.8),
    path("M13 6 L31 30 M51 6 L33 30").w(2.8),
    circle(13.0, 6.0, 2.2).filled(),
    circle(51.0, 6.0, 2.2).filled(),
];

const DRUM_MACHINE: &[Part] = &[
    circle(14.0, 12.0, 3.6),
    circle(26.0, 12.0, 3.6),
    circle(38.0, 12.0, 3.6),
    circle(50.0, 12.0, 3.6),
    rect(9.0, 22.0, 10.0, 10.0, 2.0),
    rect(21.0, 22.0, 10.0, 10.0, 2.0),
    rect(33.0, 22.0, 10.0, 10.0, 2.0),
    rect(45.0, 22.0, 10.0, 10.0, 2.0).filled(),
    rect(9.0, 34.0, 10.0, 10.0, 2.0),
    rect(21.0, 34.0, 10.0, 10.0, 2.0).filled(),
    rect(33.0, 34.0, 10.0, 10.0, 2.0),
    rect(45.0, 34.0, 10.0, 10.0, 2.0),
    rect(9.0, 46.0, 10.0, 10.0, 2.0).filled(),
    rect(21.0, 46.0, 10.0, 10.0, 2.0),
    rect(33.0, 46.0, 10.0, 10.0, 2.0).filled(),
    rect(45.0, 46.0, 10.0, 10.0, 2.0),
];

const SAMPLER: &[Part] = &[
    rect(7.0, 9.0, 50.0, 31.0, 3.0),
    path(
        "M16 21 V28 M20 16 V33 M24 19 V30 M28 13 V36 M32 18 V31 M36 21 V28 M40 17 V32 \
         M44 22 V27 M48 20 V29 M52 23.5 V25.5",
    )
    .w(2.4),
    rect(7.0, 45.0, 50.0, 11.0, 2.0),
    path("M17 45 V56 M27 45 V56 M37 45 V56 M47 45 V56").w(1.6),
];

const SAMPLE_BANK: &[Part] = &[
    path("M16 15 V9 H57 V41 H51").a(0.55),
    path("M11 22 V16 H52 V48 H46").a(0.8),
    rect(6.0, 23.0, 40.0, 32.0, 3.0),
    path(
        "M10 39 H14 L16.5 31 L19.5 47 L22.5 28 L25.5 50 L28.5 33 L31.5 44 L34.5 35 L37.5 41 L42 39",
    ),
];

const AUDIO: &[Part] = &[path(
    "M10 30 V34 M15 25 V39 M20 18 V46 M25 22 V42 M30 12 V52 M35 20 V44 M40 26 V38 \
     M45 16 V48 M50 24 V40 M55 29 V35",
)
.w(3.0)];

const MICROPHONE: &[Part] = &[
    rect(24.0, 7.0, 16.0, 28.0, 8.0),
    path("M17 29 C17 47 47 47 47 29"),
    path("M32 41 V54 M23 55 H41"),
    path("M24 17 H30 M24 23 H30 M24 29 H30").w(1.6).a(0.7),
];

const PIANO: &[Part] = &[
    path("M8 32 C16 14 40 8 56 19"),
    path("M41 32 L45 14.5").w(2.0),
    rect(8.0, 32.0, 48.0, 20.0, 2.0),
    path("M14 32 V52 M20 32 V52 M26 32 V52 M32 32 V52 M38 32 V52 M44 32 V52 M50 32 V52").w(1.6),
    rect(12.2, 32.0, 3.6, 11.0, 0.6).filled(),
    rect(18.2, 32.0, 3.6, 11.0, 0.6).filled(),
    rect(30.2, 32.0, 3.6, 11.0, 0.6).filled(),
    rect(36.2, 32.0, 3.6, 11.0, 0.6).filled(),
    rect(42.2, 32.0, 3.6, 11.0, 0.6).filled(),
];

const STRINGS: &[Part] = &[
    path(
        "M32 22 C25 22 23.5 27 26 30.5 C21.5 33 21 41 25 45 C28 49 36 49 39 45 \
         C43 41 42.5 33 38 30.5 C40.5 27 39 22 32 22 Z",
    )
    .turned(-32.0),
    path("M32 22 V9").turned(-32.0),
    circle(32.0, 6.6, 2.3).turned(-32.0),
    path("M28 39.5 H36").turned(-32.0),
    path("M27.6 33.5 Q26 36.1 27.6 38.7 M36.4 33.5 Q38 36.1 36.4 38.7")
        .w(2.0)
        .turned(-32.0),
    path("M13 55 L56 22").w(2.2),
];

const HAND_DRUM: &[Part] = &[
    path("M14 18 C26 12 38 13 50 20.5"),
    path("M14 46 C26 52 38 51 50 43.5"),
    path("M18 25 C28 21.5 38 22.5 46 27.5 M18 32 H46 M18 39 C28 42.5 38 41.5 46 36.5")
        .w(1.5)
        .a(0.75),
    ellipse(14.0, 32.0, 5.5, 14.0),
    ellipse(50.0, 32.0, 4.6, 11.5),
    circle(50.0, 32.0, 3.6).filled(),
];

const RECORD: &[Part] = &[
    circle(30.0, 34.0, 24.0),
    circle(30.0, 34.0, 18.5).w(1.1).a(0.55),
    circle(30.0, 34.0, 14.0).w(1.1).a(0.55),
    circle(30.0, 34.0, 8.0).filled().a(0.92),
    circle(30.0, 34.0, 1.8).hole(),
    path("M56 6 L52 12 L40 28").w(2.6),
    circle(56.0, 6.0, 3.0),
];

const GROOVEBOX: &[Part] = &[
    rect(6.0, 12.0, 52.0, 41.0, 4.0),
    rect(11.0, 17.0, 19.0, 9.0, 1.5).filled().a(0.35),
    rect(11.0, 17.0, 19.0, 9.0, 1.5),
    path("M37 17 V28 M42 17 V28 M47 17 V28 M52 17 V28").w(1.6),
    rect(35.0, 20.0, 4.0, 2.6, 0.6).filled(),
    rect(40.0, 24.0, 4.0, 2.6, 0.6).filled(),
    rect(45.0, 18.0, 4.0, 2.6, 0.6).filled(),
    rect(50.0, 22.0, 4.0, 2.6, 0.6).filled(),
    rect(11.0, 32.0, 9.0, 7.0, 1.5),
    rect(22.5, 32.0, 9.0, 7.0, 1.5),
    rect(34.0, 32.0, 9.0, 7.0, 1.5).filled(),
    rect(45.5, 32.0, 8.0, 7.0, 1.5),
    rect(11.0, 42.0, 9.0, 7.0, 1.5).filled(),
    rect(22.5, 42.0, 9.0, 7.0, 1.5),
    rect(34.0, 42.0, 9.0, 7.0, 1.5),
    rect(45.5, 42.0, 8.0, 7.0, 1.5),
];

const NOISE: &[Part] = &[
    path(
        "M5 32 H11 L14.5 22 L18.5 44 L22.5 15 L27 49 L31 25 L35 39 L39 19 L43 43 L47 27 L51 35 L54 32 H59",
    ),
    circle(11.0, 13.0, 1.8).filled().a(0.85),
    circle(19.0, 54.0, 1.6).filled().a(0.85),
    circle(31.0, 9.0, 1.5).filled().a(0.85),
    circle(45.0, 55.0, 1.8).filled().a(0.85),
    circle(53.0, 14.0, 1.6).filled().a(0.85),
    circle(57.0, 47.0, 1.4).filled().a(0.85),
    circle(7.0, 47.0, 1.5).filled().a(0.85),
    circle(39.0, 7.0, 1.2).filled().a(0.85),
    circle(27.0, 57.0, 1.2).filled().a(0.85),
];

const SPEAKER: &[Part] = &[
    path("M10 25 H20 L34 13 V51 L20 39 H10 Z"),
    path("M42 23 C46.5 27.5 46.5 36.5 42 41"),
    path("M48 17 C56 25 56 39 48 47").a(0.75),
];

const FOLDER: &[Part] = &[
    path("M8 18 H25 L30 23 H56 V50 H8 Z"),
    path("M15 38 H19 L21.5 31 L24.5 45 L27.5 29 L30.5 43 L33.5 35 L36.5 40 L39 38 H49").w(2.4),
];

fn parts(symbol: Symbol) -> &'static [Part] {
    match symbol {
        Symbol::Synth => SYNTH,
        Symbol::AcidBass => ACID_BASS,
        Symbol::Drums => DRUMS,
        Symbol::DrumMachine => DRUM_MACHINE,
        Symbol::Sampler => SAMPLER,
        Symbol::SampleBank => SAMPLE_BANK,
        Symbol::Audio => AUDIO,
        Symbol::Microphone => MICROPHONE,
        Symbol::Piano => PIANO,
        Symbol::Strings => STRINGS,
        Symbol::HandDrum => HAND_DRUM,
        Symbol::Record => RECORD,
        Symbol::Groovebox => GROOVEBOX,
        Symbol::Noise => NOISE,
        Symbol::Speaker => SPEAKER,
        Symbol::Folder => FOLDER,
    }
}

/// One stroke of a path, in units: its points and whether it closes.
type Line = (Vec<Pos2>, bool);

/// Points along each curve of a path.
const CURVE_STEPS: usize = 12;

/// Turns absolute SVG path data into lines.
fn parse_path(d: &str) -> Vec<Line> {
    let mut tokens = Vec::new();
    let mut number = String::new();
    let flush = |number: &mut String, tokens: &mut Vec<Token>| {
        if !number.is_empty() {
            tokens.push(Token::Number(number.parse().unwrap_or(0.0)));
            number.clear();
        }
    };
    for c in d.chars() {
        if c.is_ascii_alphabetic() {
            flush(&mut number, &mut tokens);
            tokens.push(Token::Command(c));
        } else if c == '-' && !number.is_empty() {
            flush(&mut number, &mut tokens);
            number.push(c);
        } else if c.is_ascii_digit() || c == '.' || c == '-' {
            number.push(c);
        } else {
            flush(&mut number, &mut tokens);
        }
    }
    flush(&mut number, &mut tokens);

    let mut lines: Vec<Line> = Vec::new();
    let mut current: Vec<Pos2> = Vec::new();
    let mut at = Pos2::ZERO;
    let mut command = 'M';
    let mut i = 0;
    let take = |i: &mut usize| -> f32 {
        let v = match tokens.get(*i) {
            Some(Token::Number(v)) => *v,
            _ => 0.0,
        };
        *i += 1;
        v
    };
    while i < tokens.len() {
        if let Token::Command(c) = tokens[i] {
            command = c;
            i += 1;
            if c == 'Z' || c == 'z' {
                if current.len() > 1 {
                    lines.push((std::mem::take(&mut current), true));
                }
                continue;
            }
        }
        match command {
            'M' => {
                if current.len() > 1 {
                    lines.push((std::mem::take(&mut current), false));
                }
                current.clear();
                at = pos2(take(&mut i), take(&mut i));
                current.push(at);
                // Coordinates after a move draw lines.
                command = 'L';
            }
            'L' => {
                at = pos2(take(&mut i), take(&mut i));
                current.push(at);
            }
            'H' => {
                at.x = take(&mut i);
                current.push(at);
            }
            'V' => {
                at.y = take(&mut i);
                current.push(at);
            }
            'C' => {
                let (c1, c2) = (
                    pos2(take(&mut i), take(&mut i)),
                    pos2(take(&mut i), take(&mut i)),
                );
                let end = pos2(take(&mut i), take(&mut i));
                let start = at;
                for s in 1..=CURVE_STEPS {
                    let t = s as f32 / CURVE_STEPS as f32;
                    let u = 1.0 - t;
                    let p = start.to_vec2() * (u * u * u)
                        + c1.to_vec2() * (3.0 * u * u * t)
                        + c2.to_vec2() * (3.0 * u * t * t)
                        + end.to_vec2() * (t * t * t);
                    current.push(p.to_pos2());
                }
                at = end;
            }
            'Q' => {
                let c1 = pos2(take(&mut i), take(&mut i));
                let end = pos2(take(&mut i), take(&mut i));
                let start = at;
                for s in 1..=CURVE_STEPS {
                    let t = s as f32 / CURVE_STEPS as f32;
                    let u = 1.0 - t;
                    let p = start.to_vec2() * (u * u)
                        + c1.to_vec2() * (2.0 * u * t)
                        + end.to_vec2() * (t * t);
                    current.push(p.to_pos2());
                }
                at = end;
            }
            // Anything else is not used by the symbols; skip a number.
            _ => i += 1,
        }
    }
    if current.len() > 1 {
        lines.push((current, false));
    }
    lines
}

#[derive(Clone, Copy, Debug)]
enum Token {
    Command(char),
    Number(f32),
}

/// Each path of each symbol, parsed once.
fn parsed(d: &'static str) -> &'static [Line] {
    static CACHE: OnceLock<HashMap<&'static str, Vec<Line>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| {
        Symbol::ALL
            .iter()
            .flat_map(|s| parts(*s))
            .filter_map(|p| match p.form {
                Form::Path(d) => Some((d, parse_path(d))),
                _ => None,
            })
            .collect()
    });
    cache.get(d).map_or(&[], Vec::as_slice)
}

fn turn(p: Pos2, degrees: f32) -> Pos2 {
    if degrees == 0.0 {
        return p;
    }
    let (sin, cos) = degrees.to_radians().sin_cos();
    let v = p - pos2(32.0, 32.0);
    pos2(32.0 + v.x * cos - v.y * sin, 32.0 + v.x * sin + v.y * cos)
}

/// Draws `symbol` in `square`, in `color`. Small symbols get thicker lines
/// so they stay readable.
pub fn paint(painter: &Painter, square: Rect, symbol: Symbol, color: Color32) {
    let scale = square.width() / 64.0;
    let small = square.width() <= 24.0;
    let normal = if small { 4.2 } else { 3.2 };
    let at = |p: Pos2| square.min + p.to_vec2() * scale;
    for part in parts(symbol) {
        let width = if part.width > 0.0 { part.width } else { normal };
        let width = if small {
            width.max(normal * 0.6)
        } else {
            width
        };
        let stroke = Stroke::new((width * scale).max(0.8), color.gamma_multiply(part.alpha));
        let fill = if part.dark {
            Color32::from_black_alpha(128)
        } else {
            color.gamma_multiply(part.alpha)
        };
        match part.form {
            Form::Path(d) => {
                for (points, closed) in parsed(d) {
                    let points: Vec<Pos2> =
                        points.iter().map(|p| at(turn(*p, part.turn))).collect();
                    if *closed {
                        painter.add(Shape::closed_line(points, stroke));
                    } else {
                        painter.add(Shape::line(points, stroke));
                    }
                }
            }
            Form::Circle(x, y, r) => {
                let c = at(turn(pos2(x, y), part.turn));
                if part.fill {
                    painter.circle_filled(c, r * scale, fill);
                } else {
                    painter.circle_stroke(c, r * scale, stroke);
                }
            }
            Form::Ellipse(x, y, rx, ry) => {
                let c = at(turn(pos2(x, y), part.turn));
                let radius = vec2(rx, ry) * scale;
                if part.fill {
                    painter.add(Shape::ellipse_filled(c, radius, fill));
                } else {
                    painter.add(Shape::ellipse_stroke(c, radius, stroke));
                }
            }
            Form::Rect(x, y, w, h, r) => {
                let rect = Rect::from_min_size(at(pos2(x, y)), Vec2::new(w, h) * scale);
                let cr = CornerRadius::same((r * scale).round().clamp(0.0, 255.0) as u8);
                if part.fill {
                    painter.add(RectShape::filled(rect, cr, fill));
                } else {
                    painter.add(RectShape::stroke(rect, cr, stroke, StrokeKind::Middle));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_parse_into_lines() {
        let lines = parse_path("M14 32 V52 M20 32 V52");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].0, vec![pos2(14.0, 32.0), pos2(14.0, 52.0)]);
        let closed = parse_path("M10 25 H20 L34 13 V51 L20 39 H10 Z");
        assert_eq!(closed.len(), 1);
        assert!(closed[0].1);
        assert_eq!(closed[0].0.len(), 6);
        let curve = parse_path("M8 32 C16 14 40 8 56 19");
        assert_eq!(curve[0].0.len(), 1 + CURVE_STEPS);
        assert_eq!(*curve[0].0.last().unwrap(), pos2(56.0, 19.0));
        let negative = parse_path("M5 47 C11 47 12.5 29 17 47 L-1 -2.5");
        assert_eq!(*negative[0].0.last().unwrap(), pos2(-1.0, -2.5));
    }

    #[test]
    fn every_symbol_stays_inside_its_square() {
        for symbol in Symbol::ALL {
            assert!(!parts(symbol).is_empty(), "{symbol:?}");
            for part in parts(symbol) {
                if let Form::Path(d) = part.form {
                    let lines = parsed(d);
                    assert!(!lines.is_empty(), "{symbol:?}: {d}");
                    for (points, _) in lines {
                        for p in points {
                            let p = turn(*p, part.turn);
                            assert!(
                                (0.0..=64.0).contains(&p.x) && (0.0..=64.0).contains(&p.y),
                                "{symbol:?}: {p:?} in {d}"
                            );
                        }
                    }
                }
            }
        }
    }
}
