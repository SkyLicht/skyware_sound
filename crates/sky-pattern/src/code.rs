// SPDX-License-Identifier: GPL-3.0-or-later
//! Patterns written as Strudel code, such as
//! `s("bd*2 sd").bank("RolandTR909").every(4, rev)`: a small reader for the
//! part of JavaScript that Strudel code uses, and the functions it calls.
//! The functions follow Strudel's documented behavior; none of Strudel's
//! own code is used.
//!
//! Double-quoted and backtick strings are mini-notation, read in place so
//! that every event still points at its atom in the code. Single-quoted
//! strings are plain text, as in Strudel.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use num_traits::{CheckedAdd, CheckedDiv, CheckedMul, CheckedSub, Signed};

use crate::ast::{Kind, Node, Q, euclid_hits, map_atoms};
use crate::bounds::{MAX_EVENTS_PER_CYCLE, Problem};
use crate::controls::Control;
use crate::ops::{Arg, Op, Scale, f64_of, q_of};
use crate::query::to_time;
use crate::signal::{MAX_STEPS, Signal, Step, Wave};
use crate::{ParseError, Span, Value, Warning, parser};

/// Whether `src` is Strudel code rather than mini-notation. Mini-notation
/// never has quotes, `$:`, `;` or `=`, its dots are only decimal points,
/// and its brackets only hold numbers, so a method call such as `.fast(`
/// or a call such as `note(run(4))` marks code too.
pub fn is_code(src: &str) -> bool {
    src.contains(['"', '\'', '`', ';', '='])
        || src.contains("$:")
        || has_call(src)
        || src
            .as_bytes()
            .windows(2)
            .any(|w| w[0] == b'.' && (w[1].is_ascii_alphabetic() || w[1] == b'_'))
}

/// Functions that are called rather than chained, as in `stack(a, b)`.
const CALLS: &[&str] = &[
    "s",
    "sound",
    "note",
    "n",
    "pure",
    "mini",
    "m",
    "stack",
    "polyrhythm",
    "pr",
    "cat",
    "slowcat",
    "seq",
    "sequence",
    "fastcat",
    "randcat",
    "chooseCycles",
    "timecat",
    "stepcat",
    "arrange",
    "run",
    "irand",
    "samples",
    "hush",
    "setcps",
    "setcpm",
    "setCps",
    "setCpm",
];

/// Whether `src` calls a Strudel function, as in `note(run(4))`, or has
/// empty brackets, as in `rev()`. In mini-notation a sound before a
/// bracket starts a Euclidean rhythm, so drum names don't count.
fn has_call(src: &str) -> bool {
    let b = src.as_bytes();
    src.match_indices('(').any(|(i, _)| {
        if src[i + 1..].trim_start().starts_with(')') {
            return true;
        }
        let start = b[..i]
            .iter()
            .rposition(|c| !(c.is_ascii_alphanumeric() || *c == b'_'))
            .map_or(0, |p| p + 1);
        let name = &src[start..i];
        (CALLS.contains(&name) || arity(name).is_some()) && crate::drum_note(name).is_none()
    })
}

/// The parts of Strudel code that are not mini-notation: everything except
/// the text inside double-quoted and backtick strings. Empty for
/// mini-notation. Unlike [`crate::parse`] it never fails, so code being
/// typed still gets its colors.
pub fn code_ranges(src: &str) -> Vec<Range<usize>> {
    if !is_code(src) {
        return Vec::new();
    }
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i = src[i + 2..].find("*/").map_or(b.len(), |k| i + 2 + k + 2);
            }
            q @ (b'"' | b'`' | b'\'') => {
                let mut end = i + 1;
                while end < b.len() && b[end] != q && (q == b'`' || b[end] != b'\n') {
                    end += 1;
                }
                if q != b'\'' {
                    out.push(start..i + 1);
                    start = end;
                }
                i = (end + 1).min(b.len());
            }
            _ => i += 1,
        }
    }
    out.push(start..b.len());
    out.retain(|r| !r.is_empty());
    out
}

// --- Tokens ---

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum T {
    Ident,
    Num,
    Str,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    Dot,
    Semi,
    Colon,
    Arrow,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Assign,
    Eof,
}

#[derive(Clone, Copy, Debug)]
struct Tok {
    t: T,
    span: Span,
}

fn span(start: usize, end: usize) -> Span {
    Span { start, end }
}

fn lex(src: &str) -> Result<Vec<Tok>, ParseError> {
    let b = src.as_bytes();
    let err =
        |start: usize, end: usize, message: String| ParseError::new(src, span(start, end), message);
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        let t = match b[i] {
            b' ' | b'\t' | b'\n' | b'\r' => {
                i += 1;
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => match src[i + 2..].find("*/") {
                Some(k) => {
                    i += 2 + k + 2;
                    continue;
                }
                None => return Err(err(i, i + 2, "unclosed comment opened here".into())),
            },
            q @ (b'"' | b'\'' | b'`') => {
                i += 1;
                loop {
                    match b.get(i) {
                        None => {
                            return Err(err(
                                start,
                                start + 1,
                                "unclosed string opened here".into(),
                            ));
                        }
                        Some(&c) if c == q => {
                            i += 1;
                            break;
                        }
                        Some(b'\\') => {
                            return Err(err(
                                i,
                                i + 1,
                                "escapes such as \\n aren't supported in pattern strings".into(),
                            ));
                        }
                        Some(b'\n') if q != b'`' => {
                            return Err(err(
                                start,
                                start + 1,
                                format!(
                                    "this string isn't closed on its line; end it with {}, or \
                                     use backticks for text over several lines",
                                    q as char
                                ),
                            ));
                        }
                        Some(b'$') if q == b'`' && b.get(i + 1) == Some(&b'{') => {
                            return Err(err(i, i + 2, "${...} in strings isn't supported".into()));
                        }
                        Some(_) => i += 1,
                    }
                }
                T::Str
            }
            b'0'..=b'9' => {
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
                if b.get(i) == Some(&b'.') && b.get(i + 1).is_some_and(u8::is_ascii_digit) {
                    i += 1;
                    while i < b.len() && b[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                T::Num
            }
            b'.' if b.get(i + 1).is_some_and(u8::is_ascii_digit) => {
                i += 1;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
                T::Num
            }
            c if c.is_ascii_alphabetic() || c == b'_' || c == b'$' => {
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'$')
                {
                    i += 1;
                }
                T::Ident
            }
            b'=' if b.get(i + 1) == Some(&b'>') => {
                i += 2;
                T::Arrow
            }
            b'=' if b.get(i + 1) == Some(&b'=') => {
                return Err(err(
                    i,
                    i + 2,
                    "comparisons aren't supported in patterns".into(),
                ));
            }
            b'{' | b'}' => {
                return Err(err(
                    i,
                    i + 1,
                    "braces aren't supported here; write a samples({ ... }) map on its own line"
                        .into(),
                ));
            }
            c => {
                i += 1;
                match c {
                    b'(' => T::LParen,
                    b')' => T::RParen,
                    b'[' => T::LBracket,
                    b']' => T::RBracket,
                    b',' => T::Comma,
                    b'.' => T::Dot,
                    b';' => T::Semi,
                    b':' => T::Colon,
                    b'+' => T::Plus,
                    b'-' => T::Minus,
                    b'*' => T::Star,
                    b'/' => T::Slash,
                    b'%' => T::Percent,
                    b'=' => T::Assign,
                    _ => {
                        let ch = src[start..].chars().next().unwrap_or_default();
                        return Err(err(
                            start,
                            start + ch.len_utf8(),
                            format!("unexpected character '{ch}'"),
                        ));
                    }
                }
            }
        };
        out.push(Tok {
            t,
            span: span(start, i),
        });
    }
    out.push(Tok {
        t: T::Eof,
        span: span(b.len(), b.len()),
    });
    Ok(out)
}

// --- Syntax ---

#[derive(Debug)]
enum Expr {
    Num(Q, Span),
    /// A string literal, quotes included.
    Str(Span),
    Ident(Span),
    Call {
        name: Span,
        args: Vec<Expr>,
        span: Span,
    },
    Method {
        target: Box<Expr>,
        name: Span,
        args: Vec<Expr>,
        span: Span,
    },
    Arrow {
        param: Option<Span>,
        body: Box<Expr>,
        span: Span,
    },
    Binary {
        op: T,
        left: Box<Expr>,
        right: Box<Expr>,
        span: Span,
    },
    Neg(Box<Expr>, Span),
    List(Vec<Expr>, Span),
}

impl Expr {
    fn span(&self) -> Span {
        match self {
            Expr::Num(_, s)
            | Expr::Str(s)
            | Expr::Ident(s)
            | Expr::Neg(_, s)
            | Expr::List(_, s) => *s,
            Expr::Call { span, .. }
            | Expr::Method { span, .. }
            | Expr::Arrow { span, .. }
            | Expr::Binary { span, .. } => *span,
        }
    }
}

enum Stmt {
    Let {
        name: Span,
        value: Expr,
    },
    /// A `$:` or `name:` line; muted when the label starts with `_`.
    Play {
        muted: bool,
        expr: Expr,
    },
    Expr(Expr),
}

/// Deepest nesting of expressions.
const MAX_DEPTH: usize = 64;
/// Most decimal places a number keeps.
const MAX_DECIMALS: usize = 6;

struct Parser<'a> {
    src: &'a str,
    toks: Vec<Tok>,
    pos: usize,
    depth: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Tok {
        self.peek_at(0)
    }

    fn peek_at(&self, k: usize) -> Tok {
        self.toks[(self.pos + k).min(self.toks.len() - 1)]
    }

    fn bump(&mut self) -> Tok {
        let tok = self.peek();
        if tok.t != T::Eof {
            self.pos += 1;
        }
        tok
    }

    fn text(&self, s: Span) -> &str {
        &self.src[s.start..s.end]
    }

    fn error(&self, mut s: Span, message: impl Into<String>) -> ParseError {
        // The end of the code: point at its last character instead.
        if s.start == s.end && s.start > 0 {
            s.start = self.src[..s.start]
                .char_indices()
                .next_back()
                .map_or(0, |(i, _)| i);
        }
        ParseError::new(self.src, s, message)
    }

    fn describe(&self, tok: Tok) -> String {
        if tok.t == T::Eof {
            "the end of the code".into()
        } else {
            format!("'{}'", self.text(tok.span))
        }
    }

    fn expect(&mut self, t: T, what: &str) -> Result<Tok, ParseError> {
        let tok = self.peek();
        if tok.t == t {
            return Ok(self.bump());
        }
        Err(self.error(
            tok.span,
            format!("expected {what}, found {}", self.describe(tok)),
        ))
    }

    fn program(&mut self) -> Result<Vec<Stmt>, ParseError> {
        let mut out = Vec::new();
        loop {
            match self.peek().t {
                T::Eof => return Ok(out),
                T::Semi => {
                    self.bump();
                }
                _ => out.push(self.stmt()?),
            }
        }
    }

    fn stmt(&mut self) -> Result<Stmt, ParseError> {
        let tok = self.peek();
        if tok.t == T::Ident && matches!(self.text(tok.span), "const" | "let" | "var") {
            self.bump();
            let name = self.expect(T::Ident, "a name")?;
            self.expect(T::Assign, "'='")?;
            let value = self.expr()?;
            return Ok(Stmt::Let {
                name: name.span,
                value,
            });
        }
        if tok.t == T::Ident && self.peek_at(1).t == T::Colon {
            self.bump();
            self.bump();
            let muted = self.text(tok.span).starts_with('_');
            return Ok(Stmt::Play {
                muted,
                expr: self.expr()?,
            });
        }
        Ok(Stmt::Expr(self.expr()?))
    }

    fn expr(&mut self) -> Result<Expr, ParseError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.error(self.peek().span, "the code is nested too deeply"));
        }
        let e = self.arrow_or_sum();
        self.depth -= 1;
        e
    }

    fn arrow_or_sum(&mut self) -> Result<Expr, ParseError> {
        let tok = self.peek();
        let param = match (
            tok.t,
            self.peek_at(1).t,
            self.peek_at(2).t,
            self.peek_at(3).t,
        ) {
            (T::Ident, T::Arrow, ..) => Some((Some(tok.span), 2)),
            (T::LParen, T::RParen, T::Arrow, _) => Some((None, 3)),
            (T::LParen, T::Ident, T::RParen, T::Arrow) => Some((Some(self.peek_at(1).span), 4)),
            _ => None,
        };
        if let Some((param, skip)) = param {
            for _ in 0..skip {
                self.bump();
            }
            let body = self.expr()?;
            let end = body.span().end;
            return Ok(Expr::Arrow {
                param,
                body: Box::new(body),
                span: span(tok.span.start, end),
            });
        }
        let mut left = self.product()?;
        while matches!(self.peek().t, T::Plus | T::Minus) {
            let op = self.bump().t;
            let right = self.product()?;
            let s = span(left.span().start, right.span().end);
            left = Expr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
                span: s,
            };
        }
        Ok(left)
    }

    fn product(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.unary()?;
        while matches!(self.peek().t, T::Star | T::Slash | T::Percent) {
            let op = self.bump().t;
            let right = self.unary()?;
            let s = span(left.span().start, right.span().end);
            left = Expr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
                span: s,
            };
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Expr, ParseError> {
        let tok = self.peek();
        if tok.t == T::Minus {
            self.bump();
            let inner = self.unary()?;
            let s = span(tok.span.start, inner.span().end);
            return Ok(Expr::Neg(Box::new(inner), s));
        }
        if tok.t == T::Ident && self.text(tok.span) == "await" {
            self.bump();
            return self.unary();
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<Expr, ParseError> {
        let mut e = self.primary()?;
        while self.peek().t == T::Dot {
            self.bump();
            let name = self.expect(T::Ident, "a function name after '.'")?;
            let open = self.peek();
            if open.t != T::LParen {
                return Err(self.error(
                    open.span,
                    format!(
                        "expected '(' after .{}, as in .{}(2)",
                        self.text(name.span),
                        self.text(name.span)
                    ),
                ));
            }
            self.bump();
            let (args, end) = self.args(T::RParen)?;
            let s = span(e.span().start, end);
            e = Expr::Method {
                target: Box::new(e),
                name: name.span,
                args,
                span: s,
            };
        }
        Ok(e)
    }

    fn primary(&mut self) -> Result<Expr, ParseError> {
        let tok = self.bump();
        match tok.t {
            T::Num => Ok(Expr::Num(self.number(tok)?, tok.span)),
            T::Str => Ok(Expr::Str(tok.span)),
            T::Ident if self.peek().t == T::LParen => {
                self.bump();
                let (args, end) = self.args(T::RParen)?;
                Ok(Expr::Call {
                    name: tok.span,
                    args,
                    span: span(tok.span.start, end),
                })
            }
            T::Ident => Ok(Expr::Ident(tok.span)),
            T::LParen => {
                let e = self.expr()?;
                self.expect(T::RParen, "')'")?;
                Ok(e)
            }
            T::LBracket => {
                let (items, end) = self.args(T::RBracket)?;
                Ok(Expr::List(items, span(tok.span.start, end)))
            }
            _ => Err(self.error(
                tok.span,
                format!("expected a pattern, found {}", self.describe(tok)),
            )),
        }
    }

    /// Comma-separated expressions up to `close`, and the end of `close`.
    fn args(&mut self, close: T) -> Result<(Vec<Expr>, usize), ParseError> {
        let mut out = Vec::new();
        loop {
            let tok = self.peek();
            if tok.t == close {
                self.bump();
                return Ok((out, tok.span.end));
            }
            out.push(self.expr()?);
            let tok = self.peek();
            match tok.t {
                T::Comma => {
                    self.bump();
                }
                t if t == close => {}
                _ => {
                    let want = if close == T::RParen { "')'" } else { "']'" };
                    return Err(self.error(
                        tok.span,
                        format!("expected ',' or {want}, found {}", self.describe(tok)),
                    ));
                }
            }
        }
    }

    /// A number literal, exactly: `0.25` is 1/4. Places past the sixth are
    /// rounded off.
    fn number(&self, tok: Tok) -> Result<Q, ParseError> {
        let text = self.text(tok.span);
        let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
        let too_large = || self.error(tok.span, "this number is too large");
        let whole: i128 = if whole.is_empty() {
            0
        } else {
            whole.parse().map_err(|_| too_large())?
        };
        if whole > 1_000_000_000 {
            return Err(too_large());
        }
        let digits = &fraction[..fraction.len().min(MAX_DECIMALS)];
        let mut q = Q::from_integer(whole);
        if !digits.is_empty() {
            let numer: i128 = digits.parse().map_err(|_| too_large())?;
            q += Q::new(numer, 10i128.pow(digits.len() as u32));
        }
        if fraction.len() > MAX_DECIMALS && fraction.as_bytes()[MAX_DECIMALS] >= b'5' {
            q += Q::new(1, 10i128.pow(MAX_DECIMALS as u32));
        }
        Ok(q)
    }
}

// --- Values ---

#[derive(Clone)]
enum Val<'a> {
    Num(Q),
    /// A string literal, quotes included.
    Str(Span),
    Pat(Arc<Node>),
    Sig(Signal),
    Func(Rc<Func<'a>>),
    List(Vec<(Val<'a>, Span)>),
    /// What a line such as `setcps(1)` gives.
    Nothing,
}

enum Func<'a> {
    Arrow {
        param: Option<String>,
        body: &'a Expr,
        env: Rc<Env<'a>>,
    },
    /// A pattern function waiting for its pattern, such as `fast(2)` or
    /// `rev`.
    Method {
        name: String,
        args: Vec<(Val<'a>, Span)>,
    },
}

type Env<'a> = HashMap<String, Val<'a>>;
type Args<'a> = Vec<(Val<'a>, Span)>;
/// Builds a pattern from a pattern and one number, as `fast` does.
type Build<'e, 'a> = dyn FnMut(&mut Eval<'a>, Arc<Node>, Q) -> Result<Arc<Node>, ParseError> + 'e;

/// How the terms of a string are read.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Notes, note numbers and sounds, as mini-notation reads them.
    Notes,
    /// Notes only, for `note`.
    NotesOnly,
    Numbers,
    /// Every name a sound, for `s` and `bank`.
    Sounds,
    /// True and false, for `struct` and `mask`: `0`, `f`, `false` and rests
    /// are false.
    Bool,
    /// Scale names such as `C:minor`.
    Scale,
}

/// A numeric argument, fixed or following a pattern.
enum Num {
    Const(Q),
    Pattern(Arc<Node>, Vec<Q>),
}

/// Most expressions one piece of code can evaluate.
const MAX_WORK: u64 = 200_000;
/// Most nodes a query of one cycle may visit.
const MAX_COST: u64 = 1 << 20;
/// Most distinct values a patterned argument such as `fast("<1 2>")` can have.
const MAX_CASES: usize = 64;
/// Largest speed factor.
const MAX_FACTOR: i128 = 10_000;
/// Largest shift in time, in cycles.
const MAX_SHIFT: i128 = 1024;
/// Largest count for `ply`, `iter`, `every`, `run`, `segment` and the like.
const MAX_COUNT: i128 = 1024;

/// Effects Strudel applies per note; here they live on the track, so they
/// are left out with a warning.
const EFFECTS: &[&str] = &[
    "pan",
    "hpf",
    "hp",
    "hcutoff",
    "hpq",
    "hresonance",
    "bpf",
    "bp",
    "bandf",
    "bpq",
    "room",
    "size",
    "roomsize",
    "rsize",
    "sz",
    "roomfade",
    "roomlp",
    "roomdim",
    "delay",
    "delaytime",
    "delayt",
    "dt",
    "delayfeedback",
    "delayfb",
    "dfb",
    "crush",
    "coarse",
    "shape",
    "distort",
    "dist",
    "vowel",
    "orbit",
    "phaser",
    "ph",
    "phaserdepth",
    "phd",
    "phaserrate",
    "phasersweep",
    "phasercenter",
    "tremolo",
    "tremolosync",
    "tremolodepth",
    "postgain",
    "lpenv",
    "lpe",
    "lpattack",
    "lpa",
    "lpdecay",
    "lpd",
    "lpsustain",
    "lps",
    "lprelease",
    "lpr",
    "vib",
    "vibrato",
    "vibmod",
    "vmod",
    "fm",
    "fmh",
    "fmi",
    "fmattack",
    "fmdecay",
    "fmsustain",
    "fmrelease",
    "fmenv",
    "duckorbit",
    "duckattack",
    "duckdepth",
    "compressor",
    "squiz",
    "waveloss",
    "dry",
    "detune",
    "unison",
    "spread",
    "noise",
    "cut",
    "channels",
    "ftype",
    "drive",
    "djf",
    "lfo",
];

/// Functions that only draw or label in Strudel's editor.
const VISUAL: &[&str] = &[
    "color",
    "colour",
    "pianoroll",
    "_pianoroll",
    "punchcard",
    "_punchcard",
    "scope",
    "_scope",
    "tscope",
    "_tscope",
    "fscope",
    "_fscope",
    "spiral",
    "_spiral",
    "spectrum",
    "_spectrum",
    "wordfall",
    "_wordfall",
    "markcss",
    "log",
    "logValues",
    "draw",
    "play",
    "p",
    "q",
    "label",
    "analyze",
];

/// Strudel functions this engine doesn't have yet.
const UNSUPPORTED: &[&str] = &[
    "chop",
    "striate",
    "slice",
    "splice",
    "loopAt",
    "loopAtCps",
    "fit",
    "begin",
    "end",
    "linger",
    "zoom",
    "compress",
    "fastGap",
    "press",
    "pressBy",
    "invert",
    "inv",
    "when",
    "whenmod",
    "arp",
    "arpWith",
    "voicing",
    "voicings",
    "chord",
    "rootNotes",
    "pick",
    "inhabit",
    "squeeze",
    "polymeter",
    "pm",
    "echoWith",
    "stutWith",
    "plyWith",
    "wchoose",
    "wchooseCycles",
    "wrandcat",
    "cpm",
    "cps",
    "ribbon",
    "shuffle",
    "scramble",
    "bite",
    "rot",
    "midi",
    "osc",
    "freq",
    "unit",
    "binary",
    "binaryN",
    "toBipolar",
    "fromBipolar",
];

/// How many arguments a pattern function takes besides its pattern, for
/// writing it on its own, as in `every(4, fast(2))`.
fn arity(name: &str) -> Option<usize> {
    Some(match name {
        "rev" | "palindrome" | "degrade" | "undegrade" | "brak" => 0,
        "fast" | "density" | "slow" | "sparsity" | "hurry" | "early" | "late" | "ply" | "iter"
        | "iterBack" | "iterback" | "swing" | "sometimes" | "often" | "rarely" | "almostNever"
        | "almostAlways" | "always" | "never" | "someCycles" | "degradeBy" | "undegradeBy"
        | "struct" | "mask" | "superimpose" | "layer" | "jux" | "add" | "sub" | "mul" | "div"
        | "transpose" | "scale" | "s" | "sound" | "n" | "note" | "bank" | "gain" | "velocity"
        | "speed" | "lpf" | "cutoff" | "ctf" | "lp" | "resonance" | "lpq" | "attack" | "att"
        | "decay" | "dec" | "sustain" | "sus" | "release" | "rel" | "legato" | "clip" => 1,
        "every" | "firstOf" | "lastOf" | "sometimesBy" | "someCyclesBy" | "off" | "juxBy"
        | "range" | "euclid" | "swingBy" | "chunk" | "inside" | "outside" | "euclidLegato" => 2,
        "euclidRot" | "within" | "euclidLegatoRot" | "echo" | "stut" => 3,
        _ => return None,
    })
}

fn control(name: &str) -> Option<Control> {
    Some(match name {
        "gain" => Control::Gain,
        "velocity" => Control::Velocity,
        "speed" => Control::Speed,
        "lpf" | "cutoff" | "ctf" | "lp" => Control::Cutoff,
        "resonance" | "lpq" => Control::Resonance,
        "attack" | "att" => Control::Attack,
        "decay" | "dec" => Control::Decay,
        "sustain" | "sus" => Control::Sustain,
        "release" | "rel" => Control::Release,
        _ => return None,
    })
}

fn signal_wave(name: &str) -> Option<(Wave, bool)> {
    Some(match name {
        "sine" => (Wave::Sine, false),
        "sine2" => (Wave::Sine, true),
        "cosine" => (Wave::Cosine, false),
        "cosine2" => (Wave::Cosine, true),
        "saw" => (Wave::Saw, false),
        "saw2" => (Wave::Saw, true),
        "isaw" => (Wave::Isaw, false),
        "isaw2" => (Wave::Isaw, true),
        "tri" => (Wave::Tri, false),
        "tri2" => (Wave::Tri, true),
        "square" => (Wave::Square, false),
        "square2" => (Wave::Square, true),
        "rand" => (Wave::Rand, false),
        "rand2" => (Wave::Rand, true),
        "perlin" => (Wave::Perlin, false),
        _ => return None,
    })
}

fn is_zero(q: Q) -> bool {
    q == Q::from_integer(0)
}

fn atom_value(node: &Node) -> Option<&Value> {
    match &node.kind {
        Kind::Atom {
            value, controls, ..
        } if controls.is_empty() => Some(value),
        _ => None,
    }
}

struct Eval<'a> {
    src: &'a str,
    warnings: Vec<Warning>,
    /// Effects left out, each with where it was first written.
    ignored: Vec<(String, Span)>,
    juxed: bool,
    work: u64,
}

impl<'a> Eval<'a> {
    fn text(&self, s: Span) -> &'a str {
        &self.src[s.start..s.end]
    }

    fn error(&self, s: Span, message: impl Into<String>) -> ParseError {
        ParseError::new(self.src, s, message)
    }

    /// Fails when `node` can't be queried safely; `s` is the code that
    /// built it.
    fn check(&self, node: Arc<Node>, s: Span) -> Result<Arc<Node>, ParseError> {
        let message = match node.bounds.problem() {
            Some(Problem::TooFine) => "this divides time too finely to keep exact timing".into(),
            Some(Problem::TooDense) => {
                format!("this plays more than {MAX_EVENTS_PER_CYCLE} events per cycle")
            }
            None if node.cost > MAX_COST => {
                "this pattern is too complex to play; use fewer functions inside each other".into()
            }
            None => return Ok(node),
        };
        Err(self.error(s, message))
    }

    fn eval(&mut self, e: &'a Expr, env: &Env<'a>) -> Result<Val<'a>, ParseError> {
        self.work += 1;
        if self.work > MAX_WORK {
            return Err(self.error(e.span(), "this code does too much work to read"));
        }
        match e {
            Expr::Num(q, _) => Ok(Val::Num(*q)),
            Expr::Str(s) => Ok(Val::Str(*s)),
            Expr::Ident(s) => match env.get(self.text(*s)) {
                Some(v) => Ok(v.clone()),
                None => self.ident(*s),
            },
            Expr::Call { name, args, span } => {
                let args = self.eval_args(args, env)?;
                self.call(*name, args, *span)
            }
            Expr::Method {
                target,
                name,
                args,
                span,
            } => {
                let t = self.eval(target, env)?;
                let args = self.eval_args(args, env)?;
                self.method((t, target.span()), *name, args, *span)
            }
            Expr::Arrow { param, body, .. } => Ok(Val::Func(Rc::new(Func::Arrow {
                param: param.map(|p| self.text(p).to_string()),
                body,
                env: Rc::new(env.clone()),
            }))),
            Expr::Binary {
                op,
                left,
                right,
                span,
            } => {
                let (a, b) = (self.eval(left, env)?, self.eval(right, env)?);
                let (Val::Num(a), Val::Num(b)) = (a, b) else {
                    return Err(self.error(*span, "arithmetic works on plain numbers only"));
                };
                let r = match op {
                    T::Plus => a.checked_add(&b),
                    T::Minus => a.checked_sub(&b),
                    T::Star => a.checked_mul(&b),
                    T::Slash if !is_zero(b) => a.checked_div(&b),
                    T::Percent if !is_zero(b) => Some(a - (a / b).floor() * b),
                    _ => return Err(self.error(*span, "this divides by zero")),
                };
                match r {
                    Some(q) if q.numer().abs() < 1 << 62 && *q.denom() < 1 << 40 => Ok(Val::Num(q)),
                    _ => Err(self.error(*span, "this number is too large")),
                }
            }
            Expr::Neg(inner, span) => match self.eval(inner, env)? {
                Val::Num(q) => Ok(Val::Num(-q)),
                _ => Err(self.error(*span, "'-' works on plain numbers only")),
            },
            Expr::List(items, _) => Ok(Val::List(self.eval_args(items, env)?)),
        }
    }

    fn eval_args(&mut self, args: &'a [Expr], env: &Env<'a>) -> Result<Args<'a>, ParseError> {
        args.iter()
            .map(|a| Ok((self.eval(a, env)?, a.span())))
            .collect()
    }

    fn ident(&mut self, s: Span) -> Result<Val<'a>, ParseError> {
        let name = self.text(s);
        if name == "silence" {
            return Ok(Val::Pat(Node::silence()));
        }
        if let Some((wave, bipolar)) = signal_wave(name) {
            let sig = Signal::new(wave, s.start as u64);
            return Ok(Val::Sig(if bipolar { sig.bipolar() } else { sig }));
        }
        match arity(name) {
            Some(0) => Ok(Val::Func(Rc::new(Func::Method {
                name: name.to_string(),
                args: Vec::new(),
            }))),
            Some(_) => Err(self.error(
                s,
                format!("{name} needs its arguments here, as in {name}(2)"),
            )),
            None if UNSUPPORTED.contains(&name) => {
                Err(self.error(s, format!("Strudel's {name} isn't supported yet")))
            }
            None => Err(self.error(s, format!("unknown name '{name}'"))),
        }
    }

    fn count(
        &self,
        args: &Args<'a>,
        want: Range<usize>,
        name: &str,
        s: Span,
    ) -> Result<(), ParseError> {
        if want.contains(&args.len()) {
            return Ok(());
        }
        let (lo, hi) = (want.start, want.end - 1);
        let what = match (lo, hi) {
            (0, 0) => "no arguments".to_string(),
            (a, b) if a == b => format!("{a} argument{}", if a == 1 { "" } else { "s" }),
            (a, b) if b > 100 => format!("at least {a} argument{}", if a == 1 { "" } else { "s" }),
            (a, b) => format!("{a} to {b} arguments"),
        };
        Err(self.error(s, format!("{name} takes {what}, got {}", args.len())))
    }

    // --- Turning values into patterns ---

    /// Parses the mini-notation inside a string literal in place, so spans
    /// and error columns point into the code.
    fn string(&self, lit: Span, mode: Mode) -> Result<Arc<Node>, ParseError> {
        let content = span(lit.start + 1, lit.end - 1);
        let quote = self.src.as_bytes()[lit.start];
        if quote == b'\'' && mode == Mode::Notes {
            return Err(self.error(
                lit,
                "single-quoted text is plain text in Strudel; write patterns in double quotes",
            ));
        }
        let mut bytes: Vec<u8> = self
            .src
            .bytes()
            .map(|b| if b == b'\n' { b'\n' } else { b' ' })
            .collect();
        let src = self.src.as_bytes();
        if mode == Mode::Scale {
            // Scale names such as C:minor aren't mini-notation words, so
            // each stands in as a plain name of the same length.
            let mut i = content.start;
            while i < content.end {
                let word = |b: u8| b.is_ascii_alphanumeric() || b == b'#' || b == b':';
                if word(src[i]) {
                    let start = i;
                    while i < content.end && word(src[i]) {
                        i += 1;
                    }
                    if src[start..i].iter().any(u8::is_ascii_alphabetic) {
                        bytes[start] = b'x';
                        bytes[start + 1..i].fill(b'_');
                    } else {
                        bytes[start..i].copy_from_slice(&src[start..i]);
                    }
                } else {
                    bytes[i] = src[i];
                    i += 1;
                }
            }
        } else {
            bytes[content.start..content.end].copy_from_slice(&src[content.start..content.end]);
        }
        let text = String::from_utf8(bytes).expect("whole characters replaced by spaces");
        let parsed = match mode {
            Mode::Numbers => parser::parse_numbers(&text),
            Mode::Sounds => parser::parse_sounds(&text),
            _ => parser::parse(&text),
        };
        let node = parsed.map_err(|e| {
            let start = e.offset.clamp(content.start, content.end);
            let end = (e.offset + e.len).clamp(start, content.end.max(start));
            self.error(span(start, end), e.message)
        })?;
        self.convert(node, mode)
    }

    /// `node` with its atoms read in `mode`.
    fn convert(&self, node: Arc<Node>, mode: Mode) -> Result<Arc<Node>, ParseError> {
        let mut error = None;
        let src = self.src;
        let node = match mode {
            Mode::Notes | Mode::Numbers => node,
            Mode::NotesOnly => {
                let mut atoms = Vec::new();
                node.atoms(&mut HashSet::new(), &mut atoms);
                if let Some((Value::Sound { name, .. }, s)) = atoms
                    .into_iter()
                    .find(|(v, _)| matches!(v, Value::Sound { .. }))
                {
                    return Err(self.error(
                        s,
                        format!(
                            "note takes notes such as c3 or 60; '{name}' is a sound, so write \
                             s(\"{name}\")"
                        ),
                    ));
                }
                // Numbers, as from run(4), play as the nearest notes.
                map_atoms(&node, &mut |v, _| match v {
                    Value::Number(_) => v.midi_note().map(Value::Note),
                    v => Some(v.clone()),
                })
            }
            Mode::Sounds => map_atoms(&node, &mut |v, s| match v {
                Value::Note(_)
                    if src[s.start..s.end].starts_with(|c: char| c.is_ascii_alphabetic()) =>
                {
                    Some(Value::Sound {
                        name: src[s.start..s.end].to_string(),
                        index: 0,
                    })
                }
                v => Some(v.clone()),
            }),
            Mode::Bool => map_atoms(&node, &mut |v, s| {
                let text = src[s.start..s.end].to_ascii_lowercase();
                (!matches!(text.as_str(), "0" | "f" | "false")).then(|| v.clone())
            }),
            Mode::Scale => map_atoms(&node, &mut |_, s| {
                let name = &src[s.start..s.end];
                if let Err(e) = Scale::parse(name) {
                    error.get_or_insert((s, e));
                }
                Some(Value::Sound {
                    name: name.to_string(),
                    index: 0,
                })
            }),
        };
        match error {
            Some((s, e)) => Err(self.error(s, e)),
            None => Ok(node),
        }
    }

    fn pattern(&mut self, v: &Val<'a>, mode: Mode, s: Span) -> Result<Arc<Node>, ParseError> {
        match v {
            Val::Str(lit) => self.string(*lit, mode),
            Val::Pat(p) => match mode {
                Mode::Notes | Mode::Numbers => Ok(Arc::clone(p)),
                Mode::Scale => Err(self.error(s, "write the scale as text, as in \"C:minor\"")),
                _ => self.convert(Arc::clone(p), mode),
            },
            Val::Num(q) => {
                let atom = |value| Node::atom(value, s);
                match mode {
                    Mode::Numbers => Ok(atom(Value::Number(to_time(*q)))),
                    Mode::Bool if is_zero(*q) => Ok(Node::silence()),
                    Mode::Bool => Ok(atom(Value::Number(to_time(*q)))),
                    Mode::Notes | Mode::NotesOnly => {
                        match u8::try_from(q.to_integer())
                            .ok()
                            .filter(|n| q.is_integer() && *n <= 127)
                        {
                            Some(n) => Ok(atom(Value::Note(n))),
                            None => {
                                Err(self.error(s, "a note number is a whole number from 0 to 127"))
                            }
                        }
                    }
                    Mode::Sounds | Mode::Scale => {
                        Err(self.error(s, "expected a name in quotes, as in \"bd\""))
                    }
                }
            }
            Val::List(items) => {
                let mut steps = Vec::new();
                for (item, s) in items {
                    steps.push((self.pattern(item, mode, *s)?, Q::from_integer(1)));
                }
                self.check(Node::sequence(&steps), s)
            }
            Val::Sig(_) => Err(self.error(
                s,
                "a signal needs steps before it can play; add .segment(n), as in \
                 sine.segment(8)",
            )),
            Val::Func(_) => Err(self.error(s, "expected a pattern, found a function")),
            Val::Nothing => Err(self.error(s, "expected a pattern here")),
        }
    }

    /// A plain number, from a number or a one-number string such as "2".
    fn number(&mut self, v: &Val<'a>, s: Span) -> Result<Q, ParseError> {
        match self.numeric(v, s)? {
            Num::Const(q) => Ok(q),
            Num::Pattern(..) => Err(self.error(s, "expected a number here, not a pattern")),
        }
    }

    fn numeric(&mut self, v: &Val<'a>, s: Span) -> Result<Num, ParseError> {
        let node = match v {
            Val::Num(q) => return Ok(Num::Const(*q)),
            Val::Str(_) | Val::Pat(_) | Val::List(_) => self.pattern(v, Mode::Numbers, s)?,
            _ => return Err(self.error(s, "expected a number")),
        };
        let mut atoms = Vec::new();
        node.atoms(&mut HashSet::new(), &mut atoms);
        let mut values: Vec<Q> = Vec::new();
        for (value, at) in atoms {
            let q = match value {
                Value::Number(t) => Q::new(i128::from(*t.numer()), i128::from(*t.denom())),
                Value::Note(n) => Q::from_integer(i128::from(*n)),
                Value::Sound { .. } => return Err(self.error(at, "expected a number")),
            };
            if !values.contains(&q) {
                values.push(q);
            }
        }
        if let Some(Value::Number(t)) = atom_value(&node) {
            return Ok(Num::Const(Q::new(
                i128::from(*t.numer()),
                i128::from(*t.denom()),
            )));
        }
        if values.len() > MAX_CASES {
            return Err(self.error(
                s,
                format!("a pattern here can have at most {MAX_CASES} different values"),
            ));
        }
        Ok(Num::Pattern(node, values))
    }

    /// `f(p, x)`, or for a patterned `x`, `f` with each of its values in
    /// turn as the pattern plays them.
    fn patterned(
        &mut self,
        p: Arc<Node>,
        x: Num,
        f: &mut Build<'_, 'a>,
    ) -> Result<Arc<Node>, ParseError> {
        match x {
            Num::Const(q) => f(self, p, q),
            Num::Pattern(arg, values) => {
                let mut cases = Vec::new();
                for q in values {
                    cases.push((Value::Number(to_time(q)), f(self, Arc::clone(&p), q)?));
                }
                Ok(Node::switch(arg, cases))
            }
        }
    }

    fn whole(&mut self, v: &Val<'a>, s: Span, lo: i128, what: &str) -> Result<i128, ParseError> {
        let q = self.number(v, s)?;
        if !q.is_integer() || q.to_integer() < lo || q.to_integer() > MAX_COUNT {
            return Err(self.error(
                s,
                format!("{what} is a whole number from {lo} to {MAX_COUNT}"),
            ));
        }
        Ok(q.to_integer())
    }

    fn probability(&mut self, v: &Val<'a>, s: Span) -> Result<f64, ParseError> {
        let q = self.number(v, s)?;
        let p = f64_of(q);
        if !(0.0..=1.0).contains(&p) {
            return Err(self.error(s, "a probability is from 0 to 1"));
        }
        Ok(p)
    }

    fn func(&self, v: &Val<'a>, s: Span) -> Result<Rc<Func<'a>>, ParseError> {
        match v {
            Val::Func(f) => Ok(Rc::clone(f)),
            _ => Err(self.error(
                s,
                "expected a function, such as rev, fast(2) or x => x.fast(2)",
            )),
        }
    }

    fn apply(&mut self, f: &Func<'a>, p: Arc<Node>, s: Span) -> Result<Arc<Node>, ParseError> {
        match f {
            Func::Method { name, args } => {
                let v = self.pattern_method(p, name, args.clone(), s, s)?;
                Ok(v)
            }
            Func::Arrow { param, body, env } => {
                let mut env = (**env).clone();
                if let Some(param) = param {
                    env.insert(param.clone(), Val::Pat(p));
                }
                let v = self.eval(body, &env)?;
                self.pattern(&v, Mode::Notes, body.span())
            }
        }
    }

    /// The argument of a value function such as `.gain(...)`.
    fn set_arg(&mut self, v: &Val<'a>, mode: Mode, s: Span) -> Result<Arg, ParseError> {
        if let Val::Sig(sig) = v {
            if matches!(mode, Mode::Numbers | Mode::Notes | Mode::NotesOnly) {
                return Ok(Arg::Signal(sig.clone()));
            }
            return Err(self.error(s, "a signal gives numbers, not names"));
        }
        if let (Val::Num(q), Mode::Numbers) = (v, mode) {
            return Ok(Arg::Const(Value::Number(to_time(*q))));
        }
        let node = self.pattern(v, mode, s)?;
        Ok(match atom_value(&node) {
            Some(value) => Arg::Const(value.clone()),
            None => Arg::Pattern(node),
        })
    }

    fn set(
        &mut self,
        p: Arc<Node>,
        op: Op,
        args: &Args<'a>,
        mode: Mode,
        name: &str,
        s: Span,
    ) -> Result<Arc<Node>, ParseError> {
        self.count(args, 1..2, name, s)?;
        let arg = self.set_arg(&args[0].0, mode, args[0].1)?;
        Ok(Node::set(p, op, arg))
    }

    fn fast(&self, p: Arc<Node>, x: Q, s: Span) -> Result<Arc<Node>, ParseError> {
        if x < Q::from_integer(0) {
            return Err(self.error(s, "a speed factor can't be negative"));
        }
        if is_zero(x) {
            return Ok(Node::silence());
        }
        if x > Q::from_integer(MAX_FACTOR) || x < Q::new(1, MAX_FACTOR) {
            return Err(self.error(
                s,
                format!("a speed factor is from 1/{MAX_FACTOR} to {MAX_FACTOR}"),
            ));
        }
        self.check(Node::fast(p, x), s)
    }

    fn shift(&self, p: Arc<Node>, by: Q, s: Span) -> Result<Arc<Node>, ParseError> {
        if by.abs() > Q::from_integer(MAX_SHIFT) {
            return Err(self.error(s, format!("a shift is at most {MAX_SHIFT} cycles")));
        }
        self.check(Node::shift(p, by), s)
    }

    /// A true/false pattern of a Euclidean rhythm; with `legato` each hit
    /// lasts until the next.
    fn euclid(&mut self, args: &Args<'a>, s: Span, legato: bool) -> Result<Arc<Node>, ParseError> {
        let k = self.whole(&args[0].0, args[0].1, 0, "the number of hits")?;
        let n = self.whole(&args[1].0, args[1].1, 1, "the number of steps")?;
        if k > n {
            return Err(self.error(s, format!("can't fit {k} hits into {n} steps")));
        }
        let rot = match args.get(2) {
            Some((v, at)) => {
                let q = self.number(v, *at)?;
                if !q.is_integer() {
                    return Err(self.error(*at, "the rotation is a whole number"));
                }
                (q.to_integer() % n) as i64
            }
            None => 0,
        };
        let hits = euclid_hits(k as usize, n as usize, rot);
        let on = Node::atom(Value::Number(to_time(Q::from_integer(1))), s);
        if !legato {
            return Ok(Node::euclid(on, &hits));
        }
        let mut steps: Vec<(Arc<Node>, Q)> = Vec::new();
        for hit in hits {
            match (hit, steps.last_mut()) {
                (true, _) => steps.push((Arc::clone(&on), Q::from_integer(1))),
                (false, Some(last)) => last.1 += Q::from_integer(1),
                (false, None) => steps.push((Node::silence(), Q::from_integer(1))),
            }
        }
        Ok(Node::sequence(&steps))
    }

    /// A pattern that is true during `[start, end)` of each cycle, or
    /// outside it with `outside`.
    fn window(&self, start: Q, end: Q, outside: bool, s: Span) -> Arc<Node> {
        let on = Node::atom(Value::Number(to_time(Q::from_integer(1))), s);
        let (a, b) = if outside {
            (Arc::clone(&on), Node::silence())
        } else {
            (Node::silence(), Arc::clone(&on))
        };
        let one = Q::from_integer(1);
        let mut steps = Vec::new();
        if start > Q::from_integer(0) {
            steps.push((Arc::clone(&a), start));
        }
        if end > start {
            steps.push((b, end - start));
        }
        if end < one {
            steps.push((a, one - end));
        }
        Node::sequence(&steps)
    }

    /// `f` applied to the part of each cycle from `start` to `end`.
    fn within(
        &mut self,
        p: Arc<Node>,
        start: Q,
        end: Q,
        f: &Func<'a>,
        s: Span,
    ) -> Result<Arc<Node>, ParseError> {
        let inner = self.apply(f, Arc::clone(&p), s)?;
        Ok(Node::stack(vec![
            Node::mask(self.window(start, end, false, s), inner),
            Node::mask(self.window(start, end, true, s), p),
        ]))
    }

    fn items(&mut self, args: &Args<'a>) -> Result<Vec<Arc<Node>>, ParseError> {
        let mut out = Vec::new();
        for (v, s) in args {
            match v {
                // `stack([a, b])` is the same as `stack(a, b)`.
                Val::List(items) if args.len() == 1 => {
                    for (v, s) in items {
                        out.push(self.pattern(v, Mode::Notes, *s)?);
                    }
                }
                v => out.push(self.pattern(v, Mode::Notes, *s)?),
            }
        }
        Ok(out)
    }

    fn cat(&self, items: Vec<Arc<Node>>, s: Span) -> Result<Arc<Node>, ParseError> {
        if items.is_empty() {
            return Ok(Node::silence());
        }
        let n = items.len() as i128;
        let steps: Vec<_> = items.into_iter().map(|p| (p, Q::from_integer(1))).collect();
        self.check(Node::fast(Node::sequence(&steps), Q::new(1, n)), s)
    }

    /// `[weight, pattern]` pairs.
    fn weighted(&mut self, args: &Args<'a>, name: &str) -> Result<Vec<(Q, Arc<Node>)>, ParseError> {
        let mut out = Vec::new();
        for (v, s) in args {
            let Val::List(pair) = v else {
                return Err(self.error(
                    *s,
                    format!("{name} takes [cycles, pattern] pairs, as in [4, s(\"bd\")]"),
                ));
            };
            if pair.len() != 2 {
                return Err(self.error(*s, "expected [cycles, pattern]"));
            }
            let w = self.number(&pair[0].0, pair[0].1)?;
            if w <= Q::from_integer(0) || w > Q::from_integer(MAX_COUNT) {
                return Err(self.error(
                    pair[0].1,
                    format!("a length is from 0 to {MAX_COUNT} cycles"),
                ));
            }
            let p = self.pattern(&pair[1].0, Mode::Notes, pair[1].1)?;
            out.push((w, p));
        }
        Ok(out)
    }

    fn call(&mut self, name_span: Span, args: Args<'a>, s: Span) -> Result<Val<'a>, ParseError> {
        let name = self.text(name_span);
        let one = |ev: &mut Self, args: &Args<'a>, mode| -> Result<Val<'a>, ParseError> {
            ev.count(args, 1..2, name, s)?;
            Ok(Val::Pat(ev.pattern(&args[0].0, mode, args[0].1)?))
        };
        let node = match name {
            "s" | "sound" => return one(self, &args, Mode::Sounds),
            "note" => return one(self, &args, Mode::NotesOnly),
            "n" => return one(self, &args, Mode::Numbers),
            "pure" | "mini" | "m" => return one(self, &args, Mode::Notes),
            "stack" | "polyrhythm" | "pr" => Node::stack(self.items(&args)?),
            "cat" | "slowcat" => {
                let items = self.items(&args)?;
                self.cat(items, s)?
            }
            "seq" | "sequence" | "fastcat" => {
                let steps: Vec<_> = self
                    .items(&args)?
                    .into_iter()
                    .map(|p| (p, Q::from_integer(1)))
                    .collect();
                Node::sequence(&steps)
            }
            "randcat" | "chooseCycles" => Node::choice(self.items(&args)?, s.start as u64),
            "timecat" | "stepcat" => {
                let steps: Vec<_> = self
                    .weighted(&args, name)?
                    .into_iter()
                    .map(|(w, p)| (p, w))
                    .collect();
                Node::sequence(&steps)
            }
            "arrange" => {
                let parts = self.weighted(&args, name)?;
                let total: Q = parts.iter().map(|(w, _)| *w).sum();
                let mut steps = Vec::new();
                for (w, p) in parts {
                    steps.push((self.fast(p, w, s)?, w));
                }
                self.fast(Node::sequence(&steps), total.recip(), s)?
            }
            "run" => {
                self.count(&args, 1..2, name, s)?;
                let n = self.whole(&args[0].0, args[0].1, 0, "run's length")?;
                let steps: Vec<_> = (0..n)
                    .map(|i| {
                        let v = Value::Number(to_time(Q::from_integer(i)));
                        (Node::atom(v, s), Q::from_integer(1))
                    })
                    .collect();
                Node::sequence(&steps)
            }
            "irand" => {
                self.count(&args, 1..2, name, s)?;
                let n = self.whole(&args[0].0, args[0].1, 1, "irand's range")?;
                return Ok(Val::Sig(Signal::new(Wave::Irand(n as u32), s.start as u64)));
            }
            "samples" | "hush" => return Ok(Val::Nothing),
            "setcps" | "setcpm" | "setCps" | "setCpm" => {
                self.warnings.push(Warning {
                    message: format!(
                        "{name} was ignored: the clip's cycle length sets the speed here"
                    ),
                    span: s,
                });
                return Ok(Val::Nothing);
            }
            _ if arity(name).is_some() => {
                let want = arity(name).unwrap_or(0);
                if args.len() <= want {
                    return Ok(Val::Func(Rc::new(Func::Method {
                        name: name.to_string(),
                        args,
                    })));
                }
                let mut args = args;
                let (target, at) = args.pop().expect("more than the arity");
                return self.method((target, at), name_span, args, s);
            }
            _ if UNSUPPORTED.contains(&name) => {
                return Err(self.error(name_span, format!("Strudel's {name} isn't supported yet")));
            }
            _ => return Err(self.error(name_span, format!("unknown function '{name}'"))),
        };
        Ok(Val::Pat(self.check(node, s)?))
    }

    fn method(
        &mut self,
        target: (Val<'a>, Span),
        name_span: Span,
        args: Args<'a>,
        s: Span,
    ) -> Result<Val<'a>, ParseError> {
        let name = self.text(name_span);
        let (t, at) = target;
        let p = match &t {
            Val::Sig(sig) => return self.signal_method(sig.clone(), name, &args, s),
            Val::Str(lit) if args.is_empty() && matches!(name, "s" | "sound" | "note" | "n") => {
                let mode = match name {
                    "note" => Mode::NotesOnly,
                    "n" => Mode::Numbers,
                    _ => Mode::Sounds,
                };
                return Ok(Val::Pat(self.string(*lit, mode)?));
            }
            Val::Num(_) | Val::Func(_) | Val::Nothing => {
                return Err(self.error(at, format!(".{name}() needs a pattern before it")));
            }
            _ => self.pattern(&t, Mode::Notes, at)?,
        };
        let node = self.pattern_method(p, name, args, s, name_span)?;
        Ok(Val::Pat(node))
    }

    fn signal_method(
        &mut self,
        mut sig: Signal,
        name: &str,
        args: &Args<'a>,
        s: Span,
    ) -> Result<Val<'a>, ParseError> {
        let num = |ev: &mut Self, i: usize| -> Result<f64, ParseError> {
            Ok(f64_of(ev.number(&args[i].0, args[i].1)?))
        };
        match name {
            "range" | "rangex" => {
                self.count(args, 2..3, name, s)?;
                let (lo, hi) = (num(self, 0)?, num(self, 1)?);
                if name == "rangex" {
                    if lo <= 0.0 || hi <= 0.0 {
                        return Err(self.error(s, "rangex needs numbers above 0"));
                    }
                    sig.steps.push(Step::Exp { lo, hi });
                } else {
                    sig.steps.push(Step::Affine {
                        mul: hi - lo,
                        add: lo,
                    });
                }
            }
            "fast" | "density" | "slow" | "sparsity" => {
                self.count(args, 1..2, name, s)?;
                let x = num(self, 0)?;
                if x <= 0.0 {
                    return Err(self.error(s, "a speed factor is above 0"));
                }
                sig = sig.fast(if matches!(name, "fast" | "density") {
                    x
                } else {
                    1.0 / x
                });
            }
            "early" | "late" => {
                self.count(args, 1..2, name, s)?;
                let x = num(self, 0)?;
                sig = sig.early(if name == "early" { x } else { -x });
            }
            "add" | "sub" | "mul" | "div" => {
                self.count(args, 1..2, name, s)?;
                let x = num(self, 0)?;
                let step = match name {
                    "add" => Step::Affine { mul: 1.0, add: x },
                    "sub" => Step::Affine { mul: 1.0, add: -x },
                    "mul" => Step::Affine { mul: x, add: 0.0 },
                    _ if x == 0.0 => return Err(self.error(s, "this divides by zero")),
                    _ => Step::Affine {
                        mul: 1.0 / x,
                        add: 0.0,
                    },
                };
                sig.steps.push(step);
            }
            "round" | "floor" => {
                self.count(args, 0..1, name, s)?;
                sig.steps.push(if name == "round" {
                    Step::Round
                } else {
                    Step::Floor
                });
            }
            "segment" | "seg" => {
                self.count(args, 1..2, name, s)?;
                let n = self.whole(&args[0].0, args[0].1, 1, "the number of steps")?;
                return Ok(Val::Pat(self.check(Node::segment(sig, n, s), s)?));
            }
            _ => {
                return Err(self.error(
                    s,
                    format!(
                        "a signal has no steps for .{name}() to work on; add .segment(n) first, \
                         as in sine.segment(8)"
                    ),
                ));
            }
        }
        if sig.steps.len() > MAX_STEPS {
            return Err(self.error(s, "this signal has too many steps"));
        }
        Ok(Val::Sig(sig))
    }

    fn pattern_method(
        &mut self,
        p: Arc<Node>,
        name: &str,
        args: Args<'a>,
        s: Span,
        name_span: Span,
    ) -> Result<Arc<Node>, ParseError> {
        if let Some(c) = control(name) {
            let node = self.set(p, Op::Control(c), &args, Mode::Numbers, name, s)?;
            return self.check(node, s);
        }
        let salt = s.start as u64;
        let node = match name {
            "fast" | "density" | "slow" | "sparsity" => {
                self.count(&args, 1..2, name, s)?;
                let x = self.numeric(&args[0].0, args[0].1)?;
                let slow = matches!(name, "slow" | "sparsity");
                self.patterned(p, x, &mut |ev, p, x| {
                    let x = if slow && !is_zero(x) { x.recip() } else { x };
                    if slow && is_zero(x) {
                        return Ok(Node::silence());
                    }
                    ev.fast(p, x, s)
                })?
            }
            "hurry" => {
                self.count(&args, 1..2, name, s)?;
                let x = self.number(&args[0].0, args[0].1)?;
                let fast = self.fast(p, x, s)?;
                Node::set(
                    fast,
                    Op::Control(Control::Speed),
                    Arg::Const(Value::Number(to_time(x))),
                )
            }
            "early" | "late" => {
                self.count(&args, 1..2, name, s)?;
                let x = self.numeric(&args[0].0, args[0].1)?;
                let early = name == "early";
                self.patterned(p, x, &mut |ev, p, x| {
                    ev.shift(p, if early { -x } else { x }, s)
                })?
            }
            "rev" => {
                self.count(&args, 0..1, name, s)?;
                Node::rev(p)
            }
            "palindrome" => {
                self.count(&args, 0..1, name, s)?;
                Node::cycles(vec![Arc::clone(&p), Node::rev(p)])
            }
            "brak" => {
                self.count(&args, 0..1, name, s)?;
                let half = Node::sequence(&[
                    (Arc::clone(&p), Q::from_integer(1)),
                    (Node::silence(), Q::from_integer(1)),
                ]);
                Node::cycles(vec![p, Node::shift(half, Q::new(1, 4))])
            }
            "ply" | "iter" | "iterBack" | "iterback" => {
                self.count(&args, 1..2, name, s)?;
                let x = self.numeric(&args[0].0, args[0].1)?;
                let at = args[0].1;
                self.patterned(p, x, &mut |ev, p, x| {
                    if !x.is_integer() || x < Q::from_integer(1) || x > Q::from_integer(MAX_COUNT) {
                        return Err(ev.error(
                            at,
                            format!("{name} takes a whole number from 1 to {MAX_COUNT}"),
                        ));
                    }
                    let n = x.to_integer();
                    Ok(match name {
                        "ply" => Node::ply(p, n),
                        "iter" => Node::iter(p, n, false),
                        _ => Node::iter(p, n, true),
                    })
                })?
            }
            "swingBy" | "swing" => {
                let (by, n) = if name == "swing" {
                    self.count(&args, 1..2, name, s)?;
                    (Q::new(1, 3), &args[0])
                } else {
                    self.count(&args, 2..3, name, s)?;
                    (self.number(&args[0].0, args[0].1)?, &args[1])
                };
                let n = self.whole(&n.0, n.1, 1, "the number of slices")?;
                if by.abs() > Q::from_integer(1) {
                    return Err(self.error(s, "a swing amount is from -1 to 1"));
                }
                Node::swing(p, by, n)
            }
            "segment" | "seg" => {
                return Err(self.error(
                    name_span,
                    "segment cuts a signal into steps, as in sine.segment(8)",
                ));
            }
            "every" | "firstOf" | "lastOf" => {
                self.count(&args, 2..3, name, s)?;
                let n = self.whole(&args[0].0, args[0].1, 1, "the number of cycles")?;
                let f = self.func(&args[1].0, args[1].1)?;
                let changed = self.apply(&f, Arc::clone(&p), s)?;
                let mut nodes = vec![p; n as usize];
                let i = if name == "lastOf" { nodes.len() - 1 } else { 0 };
                nodes[i] = changed;
                Node::cycles(nodes)
            }
            "chunk" => {
                self.count(&args, 2..3, name, s)?;
                let n = self.whole(&args[0].0, args[0].1, 1, "the number of parts")?;
                if n > 64 {
                    return Err(self.error(args[0].1, "chunk takes at most 64 parts"));
                }
                let f = self.func(&args[1].0, args[1].1)?;
                let mut nodes = Vec::new();
                for i in 0..n {
                    nodes.push(self.within(
                        Arc::clone(&p),
                        Q::new(i, n),
                        Q::new(i + 1, n),
                        &f,
                        s,
                    )?);
                }
                Node::cycles(nodes)
            }
            "within" => {
                self.count(&args, 3..4, name, s)?;
                let a = self.number(&args[0].0, args[0].1)?;
                let b = self.number(&args[1].0, args[1].1)?;
                if a < Q::from_integer(0) || b > Q::from_integer(1) || a >= b {
                    return Err(self.error(
                        s,
                        "within takes a start and end from 0 to 1, as in within(0, 0.5, f)",
                    ));
                }
                let f = self.func(&args[2].0, args[2].1)?;
                self.within(p, a, b, &f, s)?
            }
            "inside" | "outside" => {
                self.count(&args, 2..3, name, s)?;
                let n = self.number(&args[0].0, args[0].1)?;
                let f = self.func(&args[1].0, args[1].1)?;
                if n <= Q::from_integer(0) {
                    return Err(self.error(args[0].1, "the factor is above 0"));
                }
                let (into, back) = if name == "inside" {
                    (n.recip(), n)
                } else {
                    (n, n.recip())
                };
                let inner = self.fast(p, into, s)?;
                let changed = self.apply(&f, inner, s)?;
                self.fast(changed, back, s)?
            }
            "sometimesBy" | "sometimes" | "often" | "rarely" | "almostNever" | "almostAlways"
            | "always" | "never" | "someCyclesBy" | "someCycles" => {
                let (prob, f) = match name {
                    "sometimesBy" | "someCyclesBy" => {
                        self.count(&args, 2..3, name, s)?;
                        (self.probability(&args[0].0, args[0].1)?, &args[1])
                    }
                    _ => {
                        self.count(&args, 1..2, name, s)?;
                        let prob = match name {
                            "often" => 0.75,
                            "rarely" => 0.25,
                            "almostNever" => 0.1,
                            "almostAlways" => 0.9,
                            "always" => 1.0,
                            "never" => 0.0,
                            _ => 0.5,
                        };
                        (prob, &args[0])
                    }
                };
                let f = self.func(&f.0, f.1)?;
                if name.starts_with("someCycles") {
                    let yes = self.apply(&f, Arc::clone(&p), s)?;
                    Node::cycle_chance(yes, p, prob, salt)
                } else {
                    let changed = self.apply(&f, Node::undegrade(Arc::clone(&p), prob, salt), s)?;
                    Node::stack(vec![Node::degrade(p, prob, salt), changed])
                }
            }
            "degradeBy" | "degrade" | "undegradeBy" | "undegrade" => {
                let prob = if name.ends_with("By") {
                    self.count(&args, 1..2, name, s)?;
                    self.probability(&args[0].0, args[0].1)?
                } else {
                    self.count(&args, 0..1, name, s)?;
                    0.5
                };
                if name.starts_with("un") {
                    Node::undegrade(p, prob, salt)
                } else {
                    Node::degrade(p, prob, salt)
                }
            }
            "struct" | "mask" => {
                self.count(&args, 1..2, name, s)?;
                let m = self.pattern(&args[0].0, Mode::Bool, args[0].1)?;
                if name == "struct" {
                    Node::structure(m, p)
                } else {
                    Node::mask(m, p)
                }
            }
            "euclid" | "euclidRot" | "euclidLegato" | "euclidLegatoRot" => {
                let rot = name.ends_with("Rot");
                self.count(&args, if rot { 3..4 } else { 2..3 }, name, s)?;
                let m = self.euclid(&args, s, name.starts_with("euclidLegato"))?;
                Node::structure(m, p)
            }
            "superimpose" | "layer" => {
                self.count(&args, 1..usize::MAX, name, s)?;
                let mut layers = if name == "superimpose" {
                    vec![Arc::clone(&p)]
                } else {
                    Vec::new()
                };
                for (f, at) in &args {
                    let f = self.func(f, *at)?;
                    layers.push(self.apply(&f, Arc::clone(&p), s)?);
                }
                Node::stack(layers)
            }
            "off" => {
                self.count(&args, 2..3, name, s)?;
                let t = self.number(&args[0].0, args[0].1)?;
                let f = self.func(&args[1].0, args[1].1)?;
                let late = self.shift(Arc::clone(&p), t, s)?;
                let changed = self.apply(&f, late, s)?;
                Node::stack(vec![p, changed])
            }
            "jux" | "juxBy" => {
                self.count(&args, if name == "jux" { 1..2 } else { 2..3 }, name, s)?;
                let (f, at) = args.last().expect("counted");
                let f = self.func(f, *at)?;
                if !self.juxed {
                    self.juxed = true;
                    self.warnings.push(Warning {
                        message: format!(
                            "{name} plays both sides in the middle: notes can't be panned \
                             one by one here"
                        ),
                        span: name_span,
                    });
                }
                let changed = self.apply(&f, Arc::clone(&p), s)?;
                Node::stack(vec![p, changed])
            }
            "echo" | "stut" => {
                self.count(&args, 3..4, name, s)?;
                let n = self.whole(&args[0].0, args[0].1, 1, "the number of echoes")?;
                if n > 64 {
                    return Err(self.error(args[0].1, "at most 64 echoes"));
                }
                // echo(count, time, feedback); Tidal's stut(count, feedback, time).
                let (time, fb) = if name == "echo" { (1, 2) } else { (2, 1) };
                let time = self.number(&args[time].0, args[time].1)?;
                let fb = f64_of(self.number(&args[fb].0, args[fb].1)?);
                let mut layers = vec![Arc::clone(&p)];
                for i in 1..n {
                    let late = self.shift(Arc::clone(&p), time * i, s)?;
                    let level = Value::Number(to_time(q_of(fb.powi(i as i32))));
                    layers.push(Node::set(
                        late,
                        Op::Control(Control::Velocity),
                        Arg::Const(level),
                    ));
                }
                Node::stack(layers)
            }
            "stack" | "cat" | "slowcat" | "seq" | "fastcat" | "sequence" => {
                let mut items = vec![p];
                items.extend(self.items(&args)?);
                match name {
                    "stack" => Node::stack(items),
                    "cat" | "slowcat" => self.cat(items, s)?,
                    _ => {
                        let steps: Vec<_> =
                            items.into_iter().map(|p| (p, Q::from_integer(1))).collect();
                        Node::sequence(&steps)
                    }
                }
            }
            "add" | "sub" | "mul" | "div" | "transpose" => {
                let op = match name {
                    "sub" => Op::Sub,
                    "mul" => Op::Mul,
                    "div" => Op::Div,
                    _ => Op::Add,
                };
                self.set(p, op, &args, Mode::Numbers, name, s)?
            }
            "range" => {
                self.count(&args, 2..3, name, s)?;
                let lo = self.number(&args[0].0, args[0].1)?;
                let hi = self.number(&args[1].0, args[1].1)?;
                let scaled = Node::set(p, Op::Mul, Arg::Const(Value::Number(to_time(hi - lo))));
                Node::set(scaled, Op::Add, Arg::Const(Value::Number(to_time(lo))))
            }
            "scale" => {
                self.count(&args, 1..2, name, s)?;
                let arg = self.set_arg(&args[0].0, Mode::Scale, args[0].1)?;
                let mut names = BTreeSet::new();
                match &arg {
                    Arg::Const(Value::Sound { name, .. }) => {
                        names.insert(name.clone());
                    }
                    Arg::Pattern(node) => {
                        let mut atoms = Vec::new();
                        node.atoms(&mut HashSet::new(), &mut atoms);
                        for (v, _) in atoms {
                            if let Value::Sound { name, .. } = v {
                                names.insert(name.clone());
                            }
                        }
                    }
                    _ => {
                        return Err(
                            self.error(args[0].1, "write the scale as text, as in \"C:minor\"")
                        );
                    }
                }
                let table = names
                    .into_iter()
                    .map(|n| Scale::parse(&n).map(|scale| (n, scale)))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| self.error(args[0].1, e))?;
                Node::set(p, Op::Scale(Arc::new(table)), arg)
            }
            "s" | "sound" => self.set(p, Op::Sound, &args, Mode::Sounds, name, s)?,
            "n" => self.set(p, Op::Index, &args, Mode::Numbers, name, s)?,
            "note" => self.set(p, Op::Note, &args, Mode::NotesOnly, name, s)?,
            "bank" => self.set(p, Op::Bank, &args, Mode::Sounds, name, s)?,
            "legato" | "clip" => self.set(p, Op::Legato, &args, Mode::Numbers, name, s)?,
            _ if EFFECTS.contains(&name) => {
                if !self.ignored.iter().any(|(n, _)| n == name) {
                    self.ignored.push((name.to_string(), name_span));
                }
                p
            }
            _ if VISUAL.contains(&name) => p,
            _ if UNSUPPORTED.contains(&name) => {
                return Err(self.error(
                    name_span,
                    format!("Strudel's .{name}() isn't supported yet"),
                ));
            }
            _ => return Err(self.error(name_span, format!("unknown function '.{name}()'"))),
        };
        self.check(node, s)
    }
}

/// Reads Strudel code into a pattern tree, with warnings about what it
/// leaves out. With `$:` lines the labeled patterns play together;
/// otherwise the last pattern does, as in Strudel.
pub(crate) fn parse(src: &str) -> Result<(Arc<Node>, Vec<Warning>), ParseError> {
    let mut parser = Parser {
        src,
        toks: lex(src)?,
        pos: 0,
        depth: 0,
    };
    let program = parser.program()?;
    let mut ev = Eval {
        src,
        warnings: Vec::new(),
        ignored: Vec::new(),
        juxed: false,
        work: 0,
    };
    let mut env: Env = HashMap::new();
    let mut playing = Vec::new();
    let mut plain: Vec<(Arc<Node>, Span)> = Vec::new();
    for stmt in &program {
        match stmt {
            Stmt::Let { name, value } => {
                let v = ev.eval(value, &env)?;
                env.insert(src[name.start..name.end].to_string(), v);
            }
            Stmt::Play { muted, expr } => {
                let v = ev.eval(expr, &env)?;
                let p = ev.pattern(&v, Mode::Notes, expr.span())?;
                if !muted {
                    playing.push(p);
                }
            }
            Stmt::Expr(expr) => match ev.eval(expr, &env)? {
                Val::Nothing => {}
                v => {
                    let p = ev.pattern(&v, Mode::Notes, expr.span())?;
                    plain.push((p, expr.span()));
                }
            },
        }
    }
    let root = if !playing.is_empty() {
        if let Some((_, s)) = plain.first() {
            ev.warnings.push(Warning {
                message: "only the $: lines play; start this line with $: to play it too".into(),
                span: *s,
            });
        }
        Node::stack(playing)
    } else {
        let last = plain.pop();
        if let Some((_, s)) = plain.first() {
            ev.warnings.push(Warning {
                message: "only the last pattern plays; start each line with $: to play them \
                          together"
                    .into(),
                span: *s,
            });
        }
        last.map_or_else(Node::silence, |(p, _)| p)
    };
    let root = ev.check(root, span(0, src.len()))?;
    if let Some((_, first)) = ev.ignored.first() {
        let names: Vec<&str> = ev.ignored.iter().map(|(n, _)| n.as_str()).collect();
        ev.warnings.push(Warning {
            message: format!(
                "{} {} ignored: effects go on the track's inserts here",
                names.join(", "),
                if names.len() == 1 { "was" } else { "were" }
            ),
            span: *first,
        });
    }
    Ok((root, ev.warnings))
}

// --- What a pattern can play ---

/// The names a pattern argument gives: its sound atoms, or a fixed name.
fn arg_names(arg: &Arg) -> BTreeSet<String> {
    match arg {
        Arg::Const(Value::Sound { name, .. }) => BTreeSet::from([name.clone()]),
        Arg::Pattern(node) => {
            let mut atoms = Vec::new();
            node.atoms(&mut HashSet::new(), &mut atoms);
            atoms
                .into_iter()
                .filter_map(|(v, _)| match v {
                    Value::Sound { name, .. } => Some(name.clone()),
                    _ => None,
                })
                .collect()
        }
        _ => BTreeSet::new(),
    }
}

/// Every sound name `root` can play.
pub(crate) fn sounds(root: &Node) -> BTreeSet<String> {
    fn go(node: &Node, memo: &mut HashMap<*const Node, BTreeSet<String>>) -> BTreeSet<String> {
        if let Some(done) = memo.get(&(node as *const Node)) {
            return done.clone();
        }
        let out = match &node.kind {
            Kind::Atom {
                value: Value::Sound { name, .. },
                ..
            } => BTreeSet::from([name.clone()]),
            Kind::Set {
                op: Op::Sound, arg, ..
            } => arg_names(arg),
            Kind::Set {
                node,
                op: Op::Bank,
                arg,
            } => {
                let inner = go(node, memo);
                let banks = arg_names(arg);
                if banks.is_empty() {
                    inner
                } else {
                    banks
                        .iter()
                        .flat_map(|b| inner.iter().map(move |n| format!("{b}_{n}")))
                        .collect()
                }
            }
            Kind::Set { node, .. } | Kind::Struct { node, .. } | Kind::Mask { node, .. } => {
                go(node, memo)
            }
            Kind::Switch { cases, .. } => cases.iter().flat_map(|(_, n)| go(n, memo)).collect(),
            _ => node
                .children()
                .into_iter()
                .flat_map(|n| go(n, memo))
                .collect(),
        };
        memo.insert(node as *const Node, out.clone());
        out
    }
    go(root, &mut HashMap::new())
}

/// Every control `root`'s events can set.
pub(crate) fn controls(root: &Node) -> BTreeSet<Control> {
    let mut out = BTreeSet::new();
    let mut seen = HashSet::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if !seen.insert(node as *const Node) {
            continue;
        }
        match &node.kind {
            Kind::Set {
                op: Op::Control(c), ..
            } => {
                out.insert(*c);
            }
            Kind::Set {
                op: Op::Note | Op::Sound,
                ..
            } => {
                out.insert(Control::Note);
            }
            Kind::Atom { controls, .. } => out.extend(controls.iter().map(|(c, _)| c)),
            _ => {}
        }
        stack.extend(node.children().into_iter().map(|n| &**n));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telling_code_from_notation() {
        for code in [
            "s(\"bd sd\")",
            "$: s('bd')",
            "run(8).fast(2)",
            "const a = 1",
            "\"bd sd\"",
        ] {
            assert!(is_code(code), "{code}");
        }
        for notation in [
            "bd*2 sd",
            "bd(3,8)",
            "hh*1.5",
            "<a b>",
            "c4 e4 g4",
            "~ bd . sd",
        ] {
            assert!(!is_code(notation), "{notation}");
        }
    }

    #[test]
    fn code_ranges_skip_string_contents() {
        let src = "s(\"bd sd\").bank('x') // \"no\"";
        let r = code_ranges(src);
        assert_eq!(r, [0..3, 8..src.len()]);
        assert!(code_ranges("bd sd").is_empty());
        // An unclosed string runs to the end of the text.
        let src = "s(\"bd ";
        assert_eq!(code_ranges(src), vec![0..3]);
    }

    #[test]
    fn tokens() {
        let toks = lex("s(\"bd\").fast(1/2) => x // c\n.5").unwrap();
        let kinds: Vec<T> = toks.iter().map(|t| t.t).collect();
        assert_eq!(
            kinds,
            [
                T::Ident,
                T::LParen,
                T::Str,
                T::RParen,
                T::Dot,
                T::Ident,
                T::LParen,
                T::Num,
                T::Slash,
                T::Num,
                T::RParen,
                T::Arrow,
                T::Ident,
                T::Num,
                T::Eof
            ]
        );
        assert!(lex("s(\"bd").is_err());
        assert!(lex("s(\"a\\nb\")").is_err());
        assert!(lex("x == y").is_err());
        assert!(lex("{a: 1}").is_err());
    }
}
