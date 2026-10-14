// SPDX-License-Identifier: GPL-3.0-or-later
//! Recursive-descent parser from tokens to the pattern tree.
//!
//! ```text
//! pattern  = group EOF
//! group    = stack ( "|" stack )*
//! stack    = sequence ( "," sequence )*
//! sequence = ( step | "_" | "!" )*
//! step     = term modifier*
//! term     = WORD | NUMBER | "~" | "[" group "]" | "<" group ">"
//! modifier = "*" NUMBER | "/" NUMBER | "@" NUMBER | "!" NUMBER? | "?" NUMBER?
//!          | "(" NUMBER "," NUMBER ( "," "-"? NUMBER )? ")"
//! ```
//!
//! The numbers after `!` and `?` are optional, so they only count when they
//! touch the operator: `bd? 1` is `bd?` followed by the note 1.

use std::sync::Arc;

use crate::ast::{Node, Q, euclid_hits};
use crate::bounds::{MAX_EVENTS_PER_CYCLE, Problem};
use crate::lexer::{Tok, Token, lex};
use crate::value::{NoteName, drum_note, note_name};
use crate::{ParseError, Span, Value};

/// Most copies `!` can make of one step.
const MAX_REPEAT: u64 = 1024;
/// Most steps a Euclidean rhythm can have.
const MAX_EUCLID_STEPS: u64 = 1024;
/// Largest speed factor or weight.
const MAX_FACTOR: i128 = 10_000;
/// Most decimal places in a speed factor or weight.
const MAX_DECIMALS: usize = 4;

pub(crate) fn parse(src: &str) -> Result<Arc<Node>, ParseError> {
    let mut parser = Parser {
        src,
        tokens: lex(src)?,
        pos: 0,
        last_end: 0,
    };
    let group = parser.group(false)?;
    let tok = parser.peek();
    let message = match tok.kind {
        Tok::Eof => return Ok(group.node),
        Tok::RBracket => "']' has no matching '['",
        Tok::RAngle => "'>' has no matching '<'",
        Tok::RParen => "')' has no matching '('",
        _ => "unexpected token",
    };
    Err(parser.error(tok.span, message))
}

struct Parser<'a> {
    src: &'a str,
    tokens: Vec<Token>,
    pos: usize,
    /// End of the last consumed token.
    last_end: usize,
}

/// A parsed group, stack or sequence.
struct Part {
    node: Arc<Node>,
    /// Whether it contains at least one step (rests count).
    has_steps: bool,
}

impl Parser<'_> {
    fn peek(&self) -> Token {
        self.tokens[self.pos]
    }

    fn bump(&mut self) -> Token {
        let tok = self.tokens[self.pos];
        if tok.kind != Tok::Eof {
            self.pos += 1;
            self.last_end = tok.span.end;
        }
        tok
    }

    fn text(&self, span: Span) -> &str {
        &self.src[span.start..span.end]
    }

    fn error(&self, span: Span, message: impl Into<String>) -> ParseError {
        ParseError::new(self.src, span, message)
    }

    fn column(&self, span: Span) -> usize {
        self.error(span, "").column
    }

    /// Fails when `node` is too finely divided to time exactly, or plays too
    /// many events per cycle; `span` is the text that built it.
    fn check_bounds(&self, node: &Node, span: Span) -> Result<(), ParseError> {
        let message = match node.bounds.problem() {
            None => return Ok(()),
            Some(Problem::TooFine) => "this divides time too finely to keep exact timing; \
                 use fewer nested decimal speeds or weights"
                .to_string(),
            Some(Problem::TooDense) => {
                format!("this plays more than {MAX_EVENTS_PER_CYCLE} events per cycle")
            }
        };
        Err(self.error(span, message))
    }

    /// The span from byte `start` to the end of the last consumed token.
    fn span_from(&self, start: usize) -> Span {
        Span {
            start,
            end: self.last_end.max(start),
        }
    }

    /// Names a token for messages: `'sd'`, or "the end of the pattern".
    fn describe(&self, tok: Token) -> String {
        if tok.kind == Tok::Eof {
            "the end of the pattern".to_string()
        } else {
            format!("'{}'", self.text(tok.span))
        }
    }

    /// Where to point when `op` should be followed by something else: the
    /// token that came instead, or `op` itself at the end of the pattern.
    fn found_span(&self, op: Token) -> Span {
        let tok = self.peek();
        if tok.kind == Tok::Eof {
            op.span
        } else {
            tok.span
        }
    }

    /// A choice between stacks separated by `|`.
    fn group(&mut self, alternate: bool) -> Result<Part, ParseError> {
        let (mut alts, first_pipe) = self.separated(Tok::Pipe, |p| p.stack(alternate))?;
        if alts.len() == 1 {
            return Ok(alts.pop().expect("one alternative"));
        }
        let salt = first_pipe.map_or(0, |span| span.start as u64);
        let alts = alts.into_iter().map(|part| part.node).collect();
        Ok(Part {
            node: Node::choice(alts, salt),
            has_steps: true,
        })
    }

    /// Layers separated by `,`.
    fn stack(&mut self, alternate: bool) -> Result<Part, ParseError> {
        let start = self.peek().span.start;
        let (mut layers, _) = self.separated(Tok::Comma, |p| p.sequence(alternate))?;
        if layers.len() == 1 {
            return Ok(layers.pop().expect("one layer"));
        }
        let node = Node::stack(layers.into_iter().map(|part| part.node).collect());
        // Layers add up their events. (A choice can't exceed its alternatives.)
        self.check_bounds(&node, self.span_from(start))?;
        Ok(Part {
            node,
            has_steps: true,
        })
    }

    /// Items separated by `sep`. When there is more than one item, none may be
    /// empty. Also returns the first separator's span.
    fn separated(
        &mut self,
        sep: Tok,
        mut item: impl FnMut(&mut Self) -> Result<Part, ParseError>,
    ) -> Result<(Vec<Part>, Option<Span>), ParseError> {
        let mut parts = vec![item(self)?];
        let mut first = None;
        while self.peek().kind == sep {
            let tok = self.bump();
            first.get_or_insert(tok.span);
            let symbol = if sep == Tok::Pipe { "|" } else { "," };
            if !parts.last().expect("at least one part").has_steps {
                return Err(self.error(tok.span, format!("expected steps before '{symbol}'")));
            }
            let part = item(self)?;
            if !part.has_steps {
                return Err(self.error(tok.span, format!("expected steps after '{symbol}'")));
            }
            parts.push(part);
        }
        Ok((parts, first))
    }

    /// Steps up to the next `,`, `|`, closing bracket or the end. Inside `< >`
    /// (`alternate`) the sequence is slowed down so each step takes a cycle.
    fn sequence(&mut self, alternate: bool) -> Result<Part, ParseError> {
        let mut steps: Vec<(Arc<Node>, Q)> = Vec::new();
        let start = self.peek().span.start;
        loop {
            let tok = self.peek();
            match tok.kind {
                Tok::Eof | Tok::RBracket | Tok::RAngle | Tok::RParen | Tok::Comma | Tok::Pipe => {
                    break;
                }
                Tok::Underscore => {
                    self.bump();
                    let Some(last) = steps.last_mut() else {
                        return Err(self.error(tok.span, "'_' must follow a step"));
                    };
                    last.1 += Q::from_integer(1);
                }
                Tok::Bang => {
                    // Only reached after `_`; `!` right after a step is a modifier.
                    self.bump();
                    let Some(last) = steps.last().cloned() else {
                        return Err(self.error(tok.span, "'!' must follow a step"));
                    };
                    let extra = self.bang_copies(tok)?;
                    steps.extend(std::iter::repeat_n(last, extra));
                }
                Tok::Star | Tok::Slash | Tok::At | Tok::Question | Tok::LParen => {
                    let message = format!("'{}' must follow a step", self.text(tok.span));
                    return Err(self.error(tok.span, message));
                }
                Tok::Colon => {
                    return Err(self.error(
                        tok.span,
                        "':' must directly follow a sound name, as in sd:3",
                    ));
                }
                Tok::Minus => {
                    return Err(self.error(
                        tok.span,
                        "unexpected '-'; negative numbers are only allowed \
                         for a Euclidean rotation",
                    ));
                }
                Tok::Word | Tok::Number | Tok::Tilde | Tok::LBracket | Tok::LAngle => {
                    self.step(&mut steps)?;
                }
            }
        }
        let has_steps = !steps.is_empty();
        let mut node = Node::sequence(&steps);
        if alternate && has_steps {
            let total: Q = steps.iter().map(|(_, weight)| *weight).sum();
            node = Node::fast(node, total.recip());
        }
        self.check_bounds(&node, self.span_from(start))?;
        Ok(Part { node, has_steps })
    }

    /// One step with its modifiers, pushed onto `steps` once per copy.
    fn step(&mut self, steps: &mut Vec<(Arc<Node>, Q)>) -> Result<(), ParseError> {
        let tok = self.bump();
        let mut node = match tok.kind {
            Tok::Word => self.word(tok)?,
            Tok::Number => self.note_number(tok)?,
            Tok::Tilde => Node::silence(),
            Tok::LBracket => {
                let inner = self.group(false)?;
                self.close(tok, Tok::RBracket)?;
                inner.node
            }
            Tok::LAngle => {
                let inner = self.group(true)?;
                self.close(tok, Tok::RAngle)?;
                inner.node
            }
            _ => unreachable!("sequence only calls step on a term"),
        };
        let mut weight = Q::from_integer(1);
        let mut copies: u64 = 1;
        loop {
            let op = self.peek();
            match op.kind {
                Tok::Star => {
                    self.bump();
                    let factor = self.factor(op, "speed factor")?;
                    node = Node::fast(node, factor);
                    self.check_bounds(&node, self.span_from(op.span.start))?;
                }
                Tok::Slash => {
                    self.bump();
                    let factor = self.factor(op, "slow-down factor")?;
                    node = Node::fast(node, factor.recip());
                    self.check_bounds(&node, self.span_from(op.span.start))?;
                }
                Tok::At => {
                    self.bump();
                    weight = self.factor(op, "weight")?;
                }
                Tok::Bang => {
                    self.bump();
                    copies += self.bang_copies(op)? as u64;
                    if copies > MAX_REPEAT {
                        return Err(self.error(
                            op.span,
                            format!("a step can be repeated at most {MAX_REPEAT} times"),
                        ));
                    }
                }
                Tok::Question => {
                    self.bump();
                    let prob = self.probability()?;
                    node = Node::degrade(node, prob, op.span.start as u64);
                }
                Tok::LParen => {
                    self.bump();
                    node = self.euclid(op, node)?;
                    self.check_bounds(&node, self.span_from(op.span.start))?;
                }
                Tok::Colon => {
                    let message = if tok.kind == Tok::Number {
                        "a note number can't have a sample index"
                    } else {
                        "':' must directly follow a sound name, as in sd:3"
                    };
                    return Err(self.error(op.span, message));
                }
                _ => break,
            }
        }
        for _ in 0..copies {
            steps.push((Arc::clone(&node), weight));
        }
        Ok(())
    }

    /// Consumes the bracket that closes `open`.
    fn close(&mut self, open: Token, expected: Tok) -> Result<(), ParseError> {
        let tok = self.peek();
        if tok.kind == expected {
            self.bump();
            return Ok(());
        }
        let open_text = self.text(open.span);
        let close_text = if expected == Tok::RBracket { "]" } else { ">" };
        if tok.kind == Tok::Eof {
            return Err(self.error(open.span, format!("unclosed '{open_text}' opened here")));
        }
        let message = format!(
            "expected '{close_text}' to close the '{open_text}' at column {}, found {}",
            self.column(open.span),
            self.describe(tok),
        );
        Err(self.error(tok.span, message))
    }

    fn word(&self, tok: Token) -> Result<Arc<Node>, ParseError> {
        let text = self.text(tok.span);
        let (name, index) = match text.split_once(':') {
            Some((name, index)) => (name, Some(index)),
            None => (text, None),
        };
        let name_span = Span {
            start: tok.span.start,
            end: tok.span.start + name.len(),
        };
        let index_span = Span {
            start: name_span.end,
            end: tok.span.end,
        };
        // Drum names win over note names, so `cb` is the cowbell.
        let note = if drum_note(name).is_some() {
            NoteName::NotANote
        } else {
            note_name(name)
        };
        let value = match note {
            NoteName::Note(note) => {
                if index.is_some() {
                    return Err(self.error(index_span, "a note can't have a sample index"));
                }
                Value::Note(note)
            }
            NoteName::OutOfRange => {
                return Err(self.error(
                    name_span,
                    format!("note '{name}' is outside the MIDI range 0-127"),
                ));
            }
            NoteName::NotANote => {
                if let Some(at) = name.find('#') {
                    let span = Span {
                        start: tok.span.start + at,
                        end: tok.span.start + at + 1,
                    };
                    return Err(self.error(span, "'#' is only allowed in note names such as f#3"));
                }
                let index = match index {
                    None => 0,
                    Some(digits) => digits.parse().map_err(|_| {
                        let digits_span = Span {
                            start: index_span.start + 1,
                            end: index_span.end,
                        };
                        self.error(digits_span, "sample index is too large")
                    })?,
                };
                Value::Sound {
                    name: name.to_string(),
                    index,
                }
            }
        };
        Ok(Node::atom(value, tok.span))
    }

    fn note_number(&self, tok: Token) -> Result<Arc<Node>, ParseError> {
        let text = self.text(tok.span);
        if text.contains('.') {
            return Err(self.error(
                tok.span,
                "note numbers must be whole numbers; use '*' or '/' to change speed",
            ));
        }
        match text.parse::<u8>() {
            Ok(note) if note <= 127 => Ok(Node::atom(Value::Note(note), tok.span)),
            _ => Err(self.error(
                tok.span,
                format!("note number {text} is outside the MIDI range 0-127"),
            )),
        }
    }

    /// The positive decimal after `op`, such as the `1.5` in `*1.5`.
    fn factor(&mut self, op: Token, what: &str) -> Result<Q, ParseError> {
        let tok = self.peek();
        let symbol = self.text(op.span);
        if tok.kind != Tok::Number {
            let message = format!(
                "expected a number after '{symbol}', found {}",
                self.describe(tok)
            );
            return Err(self.error(self.found_span(op), message));
        }
        self.bump();
        let text = self.text(tok.span);
        let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
        if fraction.len() > MAX_DECIMALS {
            return Err(self.error(
                tok.span,
                format!("a {what} can have at most {MAX_DECIMALS} decimal places"),
            ));
        }
        let too_large = || self.error(tok.span, format!("a {what} can be at most {MAX_FACTOR}"));
        let whole: i128 = whole.parse().map_err(|_| too_large())?;
        if whole > MAX_FACTOR {
            return Err(too_large());
        }
        let denom = 10i128.pow(fraction.len() as u32);
        let fraction: i128 = if fraction.is_empty() {
            0
        } else {
            fraction.parse().map_err(|_| too_large())?
        };
        let value = Q::new(whole * denom + fraction, denom);
        if value > Q::from_integer(MAX_FACTOR) {
            return Err(too_large());
        }
        if value == Q::from_integer(0) {
            return Err(self.error(tok.span, format!("a {what} must be greater than 0")));
        }
        Ok(value)
    }

    /// How many more copies a `!` adds: one, or `n - 1` for an attached `!n`.
    fn bang_copies(&mut self, bang: Token) -> Result<usize, ParseError> {
        let tok = self.peek();
        if tok.kind != Tok::Number || tok.spaced {
            return Ok(1);
        }
        self.bump();
        let count = self.whole(tok, "the repeat count")?;
        if count == 0 {
            return Err(self.error(tok.span, "a repeat count must be at least 1"));
        }
        if count > MAX_REPEAT {
            let message = format!("a step can be repeated at most {MAX_REPEAT} times");
            return Err(self.error(
                Span {
                    start: bang.span.start,
                    end: tok.span.end,
                },
                message,
            ));
        }
        Ok((count - 1) as usize)
    }

    /// The probability attached to a `?`, or 0.5 when there is none.
    fn probability(&mut self) -> Result<f64, ParseError> {
        let tok = self.peek();
        if tok.kind != Tok::Number || tok.spaced {
            return Ok(0.5);
        }
        self.bump();
        match self.text(tok.span).parse::<f64>() {
            Ok(prob) if (0.0..=1.0).contains(&prob) => Ok(prob),
            _ => Err(self.error(tok.span, "a probability must be between 0 and 1")),
        }
    }

    /// A whole number token; `what` names it in messages ("the repeat count").
    fn whole(&self, tok: Token, what: &str) -> Result<u64, ParseError> {
        let text = self.text(tok.span);
        if text.contains('.') {
            return Err(self.error(tok.span, format!("{what} must be a whole number")));
        }
        text.parse()
            .map_err(|_| self.error(tok.span, format!("{what} is too large")))
    }

    /// A whole number argument of a Euclidean rhythm, such as "the number of
    /// steps" (`what`).
    fn euclid_arg(&mut self, open: Token, what: &str) -> Result<(u64, Token), ParseError> {
        let tok = self.peek();
        if tok.kind != Tok::Number {
            return Err(self.euclid_error(open, tok, &format!("{what}, as in bd(3,8,2)")));
        }
        self.bump();
        Ok((self.whole(tok, what)?, tok))
    }

    fn euclid_error(&self, open: Token, tok: Token, expected: &str) -> ParseError {
        if tok.kind == Tok::Eof {
            self.error(open.span, "unclosed '(' opened here")
        } else {
            let message = format!("expected {expected}, found {}", self.describe(tok));
            self.error(tok.span, message)
        }
    }

    fn expect(&mut self, open: Token, kind: Tok, expected: &str) -> Result<(), ParseError> {
        let tok = self.peek();
        if tok.kind == kind {
            self.bump();
            Ok(())
        } else {
            Err(self.euclid_error(open, tok, expected))
        }
    }

    /// The `(k,n)` or `(k,n,r)` after a step; `open` is the `(`.
    fn euclid(&mut self, open: Token, node: Arc<Node>) -> Result<Arc<Node>, ParseError> {
        let (onsets, onsets_tok) = self.euclid_arg(open, "the number of onsets")?;
        self.expect(
            open,
            Tok::Comma,
            "',' and the number of steps, as in bd(3,8)",
        )?;
        let (steps, steps_tok) = self.euclid_arg(open, "the number of steps")?;
        if steps == 0 {
            return Err(self.error(steps_tok.span, "a Euclidean rhythm needs at least 1 step"));
        }
        if steps > MAX_EUCLID_STEPS {
            return Err(self.error(
                steps_tok.span,
                format!("a Euclidean rhythm can have at most {MAX_EUCLID_STEPS} steps"),
            ));
        }
        if onsets > steps {
            return Err(self.error(
                onsets_tok.span,
                format!("can't fit {onsets} onsets into {steps} steps"),
            ));
        }
        let mut rotation = 0i64;
        if self.peek().kind == Tok::Comma {
            self.bump();
            let negative = self.peek().kind == Tok::Minus;
            if negative {
                self.bump();
            }
            let (amount, _) = self.euclid_arg(open, "the rotation")?;
            // Rotating by a multiple of `steps` changes nothing, so reduce first.
            let amount = i64::try_from(amount % steps).expect("below the step limit");
            rotation = if negative { -amount } else { amount };
        }
        self.expect(open, Tok::RParen, "')' to close the Euclidean rhythm")?;
        let hits = euclid_hits(onsets as usize, steps as usize, rotation);
        Ok(Node::euclid(node, &hits))
    }
}
