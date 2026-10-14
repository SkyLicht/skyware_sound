// SPDX-License-Identifier: GPL-3.0-or-later
//! Splits mini-notation source into tokens with exact byte spans.

use crate::{ParseError, Span};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tok {
    /// A name such as `bd`, `sd:3`, `c4` or `f#3`, including any `:index`.
    Word,
    /// Digits with an optional fraction, such as `60` or `1.5`.
    Number,
    Tilde,
    Underscore,
    LBracket,
    RBracket,
    LAngle,
    RAngle,
    LParen,
    RParen,
    Comma,
    Pipe,
    Star,
    Slash,
    Bang,
    At,
    Question,
    Colon,
    Minus,
    Eof,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Token {
    pub kind: Tok,
    pub span: Span,
    /// Whether whitespace (or the start of the text) comes right before it.
    pub spaced: bool,
}

pub(crate) fn lex(src: &str) -> Result<Vec<Token>, ParseError> {
    let bytes = src.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    let mut spaced = true;
    while i < bytes.len() {
        let start = i;
        let kind = match bytes[i] {
            b'~' => Tok::Tilde,
            b'_' => Tok::Underscore,
            b'[' => Tok::LBracket,
            b']' => Tok::RBracket,
            b'<' => Tok::LAngle,
            b'>' => Tok::RAngle,
            b'(' => Tok::LParen,
            b')' => Tok::RParen,
            b',' => Tok::Comma,
            b'|' => Tok::Pipe,
            b'*' => Tok::Star,
            b'/' => Tok::Slash,
            b'!' => Tok::Bang,
            b'@' => Tok::At,
            b'?' => Tok::Question,
            b':' => Tok::Colon,
            b'-' => Tok::Minus,
            b'a'..=b'z' | b'A'..=b'Z' => Tok::Word,
            b'0'..=b'9' => Tok::Number,
            _ => {
                let ch = src[i..].chars().next().unwrap_or_default();
                if ch.is_whitespace() {
                    spaced = true;
                    i += ch.len_utf8();
                    continue;
                }
                return Err(unexpected_char(src, i, ch));
            }
        };
        i = match kind {
            Tok::Word => word_end(src, i)?,
            Tok::Number => number_end(src, i)?,
            _ => i + 1,
        };
        tokens.push(Token {
            kind,
            span: Span { start, end: i },
            spaced,
        });
        spaced = false;
    }
    tokens.push(Token {
        kind: Tok::Eof,
        span: Span {
            start: src.len(),
            end: src.len(),
        },
        spaced,
    });
    Ok(tokens)
}

fn unexpected_char(src: &str, at: usize, ch: char) -> ParseError {
    let message = match ch {
        '{' | '}' => "polymeter braces '{ }' are not supported".to_string(),
        '"' | '\'' | '`' => format!("unexpected {ch}; write the pattern without quotes"),
        _ => format!("unexpected character '{ch}'"),
    };
    ParseError::new(src, char_span(src, at), message)
}

/// The span of the character starting at byte `at`, or an empty span at the end.
fn char_span(src: &str, at: usize) -> Span {
    let len = src[at..].chars().next().map_or(0, char::len_utf8);
    Span {
        start: at,
        end: at + len,
    }
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'#'
}

/// Scans `[a-zA-Z][a-zA-Z0-9_#]*` with an optional `:digits` sample index.
fn word_end(src: &str, start: usize) -> Result<usize, ParseError> {
    let bytes = src.as_bytes();
    let mut i = start + 1;
    while i < bytes.len() && is_word_byte(bytes[i]) {
        i += 1;
    }
    if bytes.get(i) != Some(&b':') {
        return Ok(i);
    }
    let colon = i;
    let digits = colon + 1;
    let mut end = digits;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    if end == digits {
        let found = char_span(src, digits);
        let at_char = src[digits..]
            .chars()
            .next()
            .is_some_and(|c| !c.is_whitespace());
        let span = if at_char {
            found
        } else {
            Span {
                start: colon,
                end: digits,
            }
        };
        return Err(ParseError::new(
            src,
            span,
            "expected a sample index after ':'",
        ));
    }
    if end < bytes.len() && (is_word_byte(bytes[end]) || bytes[end] == b'.') {
        let mut stop = end;
        while stop < bytes.len() && (is_word_byte(bytes[stop]) || bytes[stop] == b'.') {
            stop += 1;
        }
        let span = Span {
            start: digits,
            end: stop,
        };
        return Err(ParseError::new(
            src,
            span,
            format!(
                "invalid sample index '{}'; it must be a whole number",
                &src[digits..stop]
            ),
        ));
    }
    Ok(end)
}

/// Scans digits with an optional `.digits` fraction.
fn number_end(src: &str, start: usize) -> Result<usize, ParseError> {
    let bytes = src.as_bytes();
    let digits = |mut i: usize| {
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        i
    };
    let mut i = digits(start);
    if bytes.get(i) == Some(&b'.') {
        if !bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
            let span = Span {
                start: i,
                end: i + 1,
            };
            return Err(ParseError::new(
                src,
                span,
                "expected digits after the decimal point",
            ));
        }
        i = digits(i + 1);
    }
    if i < bytes.len() && (is_word_byte(bytes[i]) || bytes[i] == b'.') {
        let mut stop = i;
        while stop < bytes.len() && (is_word_byte(bytes[stop]) || bytes[stop] == b'.') {
            stop += 1;
        }
        let span = Span { start, end: stop };
        return Err(ParseError::new(
            src,
            span,
            format!("invalid number '{}'", &src[start..stop]),
        ));
    }
    Ok(i)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<(Tok, &str, bool)> {
        lex(src)
            .unwrap()
            .into_iter()
            .map(|t| (t.kind, &src[t.span.start..t.span.end], t.spaced))
            .collect()
    }

    #[test]
    fn tokens_and_spans() {
        use Tok::*;
        assert_eq!(
            kinds("bd:3 [sd*1.5]? f#3!2"),
            [
                (Word, "bd:3", true),
                (LBracket, "[", true),
                (Word, "sd", false),
                (Star, "*", false),
                (Number, "1.5", false),
                (RBracket, "]", false),
                (Question, "?", false),
                (Word, "f#3", true),
                (Bang, "!", false),
                (Number, "2", false),
                (Eof, "", false),
            ]
        );
        assert_eq!(
            kinds("<a b>(3,8,-1) ~ _ , |"),
            [
                (LAngle, "<", true),
                (Word, "a", false),
                (Word, "b", true),
                (RAngle, ">", false),
                (LParen, "(", false),
                (Number, "3", false),
                (Comma, ",", false),
                (Number, "8", false),
                (Comma, ",", false),
                (Minus, "-", false),
                (Number, "1", false),
                (RParen, ")", false),
                (Tilde, "~", true),
                (Underscore, "_", true),
                (Comma, ",", true),
                (Pipe, "|", true),
                (Eof, "", false),
            ]
        );
    }

    #[test]
    fn whitespace_of_all_kinds_separates() {
        let toks = kinds("bd\n\tsd\u{a0}hh");
        assert_eq!(toks.len(), 4);
        assert!(toks[1].2 && toks[2].2);
    }

    #[test]
    fn lexer_errors() {
        let err = lex("bd $").unwrap_err();
        assert_eq!((err.offset, err.len, err.column), (3, 1, 4));
        assert_eq!(err.message, "unexpected character '$'");

        let err = lex("é bd").unwrap_err();
        assert_eq!((err.offset, err.len, err.column), (0, 2, 1));

        let err = lex("bd: sd").unwrap_err();
        assert_eq!((err.offset, err.len), (2, 1));
        assert_eq!(err.message, "expected a sample index after ':'");

        let err = lex("bd:x").unwrap_err();
        assert_eq!((err.offset, err.len), (3, 1));

        let err = lex("bd:3x").unwrap_err();
        assert_eq!((err.offset, err.len), (3, 2));

        let err = lex("bd*1.").unwrap_err();
        assert_eq!((err.offset, err.len), (4, 1));

        let err = lex("3bd").unwrap_err();
        assert_eq!((err.offset, err.len), (0, 3));
        assert_eq!(err.message, "invalid number '3bd'");

        let err = lex("1.5.2").unwrap_err();
        assert_eq!((err.offset, err.len), (0, 5));

        let err = lex("{bd sd}").unwrap_err();
        assert_eq!((err.offset, err.len), (0, 1));

        let err = lex("bd . sd").unwrap_err();
        assert_eq!(err.message, "unexpected character '.'");
    }
}
