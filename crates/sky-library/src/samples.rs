// SPDX-License-Identifier: GPL-3.0-or-later
//! Strudel's `samples(...)` calls, written above a pattern as in Strudel:
//!
//! ```text
//! samples('github:tidalcycles/dirt-samples')
//! samples({ slap: 'https://cdn.freesound.org/previews/495/495416_10350281-lq.mp3' })
//! bd*4, ~ sd, hh*8
//! ```
//!
//! A call names a pack by its address or writes a sample map in place, in
//! JavaScript's object notation; a line that is only a pack's address, such
//! as `github:tidalcycles/dirt-samples`, counts as a call too.
//! [`find_samples`] finds them, so the rest of the text can be read as
//! mini-notation.

use std::ops::Range;

use serde_json::{Map, Value};
use thiserror::Error;

use crate::map::SampleMap;

/// What one call names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Samples {
    /// A pack's sample map: a `github:user/repo` shortcut, a URL or a
    /// folder.
    Address(String),
    /// A sample map written in the call.
    Map(SampleMap),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SamplesCall {
    /// Where the call is in the text, with its `await` and `;`.
    pub range: Range<usize>,
    pub samples: Samples,
}

/// A call that cannot be read, and where.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct CallError {
    pub message: String,
    pub range: Range<usize>,
}

/// How deeply lists and objects may nest in a call.
const MAX_DEPTH: usize = 16;

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$' | b'#' | b'.')
}

/// True for a line that is only a pack's address.
fn is_address_line(line: &str) -> bool {
    let lower = line.get(..8).unwrap_or(line).to_ascii_lowercase();
    (lower.starts_with("github:") || lower.starts_with("http://") || lower.starts_with("https://"))
        && !line.contains(char::is_whitespace)
}

/// The `samples(...)` calls in a pattern's text and the lines that are only
/// a pack's address, in text order. A word `samples` followed by anything
/// but a quote or `{`, such as the rhythm `samples(3,8)`, is mini-notation.
pub fn find_samples(text: &str) -> Result<Vec<SamplesCall>, CallError> {
    let bytes = text.as_bytes();
    let mut calls = Vec::new();
    let mut from = 0;
    while let Some(found) = text[from..].find("samples") {
        let start = from + found;
        let after = start + "samples".len();
        from = after;
        if start > 0 && is_word(bytes[start - 1]) || bytes.get(after).is_some_and(|&b| is_word(b)) {
            continue;
        }
        let mut r = Reader { text, at: after };
        r.blanks();
        if !r.eat(b'(') {
            continue;
        }
        r.space()?;
        if !matches!(r.peek(), Some(b'\'' | b'"' | b'`' | b'{')) {
            continue;
        }
        let samples = r.arguments(start)?;
        // Strudel's examples write `await samples(...)` and end it with `;`.
        let mut end = r.at;
        let rest = &text[end..];
        let line_rest = rest.len() - rest.trim_start_matches([' ', '\t']).len();
        if rest[line_rest..].starts_with(';') {
            end += line_rest + 1;
        }
        let before = text[..start].trim_end_matches([' ', '\t']);
        let start = match before.strip_suffix("await") {
            Some(b) if !b.bytes().last().is_some_and(is_word) && before.len() < start => b.len(),
            _ => start,
        };
        calls.push(SamplesCall {
            range: start..end,
            samples,
        });
        from = end;
    }

    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let content = line.trim();
        let begin = offset + (line.len() - line.trim_start().len());
        offset += line.len();
        if content.is_empty() || !is_address_line(content) {
            continue;
        }
        let range = begin..begin + content.len();
        if calls
            .iter()
            .any(|c| c.range.start < range.end && range.start < c.range.end)
        {
            continue;
        }
        calls.push(SamplesCall {
            samples: address(content, None, range.clone())?,
            range,
        });
    }
    calls.sort_by_key(|c| c.range.start);
    Ok(calls)
}

/// `text` with each call replaced by spaces, newlines kept, so it reads as
/// mini-notation and offsets in it are offsets in `text`.
pub fn blank_calls(text: &str, calls: &[SamplesCall]) -> String {
    let mut bytes = text.as_bytes().to_vec();
    for call in calls {
        for b in &mut bytes[call.range.clone()] {
            if !matches!(*b, b'\n' | b'\r') {
                *b = b' ';
            }
        }
    }
    // Only ASCII spaces replaced whole characters, so this is still UTF-8.
    String::from_utf8(bytes).unwrap_or_default()
}

/// `text` without the calls and the lines they leave empty.
pub fn strip_calls(text: &str, calls: &[SamplesCall]) -> String {
    if calls.is_empty() {
        return text.to_string();
    }
    let mut lines = Vec::new();
    let mut offset = 0;
    for line in text.split('\n') {
        let span = offset..offset + line.len();
        offset += line.len() + 1;
        let mut kept = String::new();
        let mut at = span.start;
        let mut touched = false;
        for call in calls {
            let (s, e) = (
                call.range.start.max(span.start),
                call.range.end.min(span.end),
            );
            if s < e || (call.range.start <= span.start && span.end <= call.range.end) {
                touched = true;
            }
            if s < e {
                kept.push_str(&text[at.max(span.start)..s.max(at)]);
                at = at.max(e);
            }
        }
        kept.push_str(&text[at.min(span.end)..span.end]);
        if touched && kept.trim().is_empty() {
            continue;
        }
        lines.push(kept);
    }
    lines.join("\n").trim().to_string()
}

/// The address in Strudel's `samples('…')`, for a field that takes a pack's
/// address and is given the call; anything else comes back trimmed.
pub fn address_in(text: &str) -> String {
    let text = text.trim();
    match find_samples(text).as_deref() {
        Ok([call]) if call.range == (0..text.len()) => match &call.samples {
            Samples::Address(a) => a.clone(),
            Samples::Map(_) => text.to_string(),
        },
        _ => text.to_string(),
    }
}

fn address(text: &str, base: Option<String>, range: Range<usize>) -> Result<Samples, CallError> {
    let err = |message: String| CallError {
        message,
        range: range.clone(),
    };
    let text = text.trim();
    if text.is_empty() {
        return Err(err("samples() needs the address of a pack".into()));
    }
    if base.is_some() {
        return Err(err(
            "a base address goes with a sample map written in samples({ ... }), not with a \
             pack's address"
                .into(),
        ));
    }
    if let Some((scheme, _)) = text.split_once(':') {
        let lower = scheme.to_ascii_lowercase();
        let known = matches!(lower.as_str(), "github" | "http" | "https" | "file");
        if scheme.len() > 1 && scheme.bytes().all(|b| b.is_ascii_alphabetic()) && !known {
            return Err(err(format!(
                "{scheme}: packs are not supported; give a github:user/repo shortcut, an \
                 https:// address of a strudel.json or a folder"
            )));
        }
    }
    Ok(Samples::Address(text.to_string()))
}

struct Reader<'a> {
    text: &'a str,
    at: usize,
}

impl Reader<'_> {
    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.at).copied()
    }

    fn eat(&mut self, b: u8) -> bool {
        let yes = self.peek() == Some(b);
        if yes {
            self.at += 1;
        }
        yes
    }

    fn error(&self, start: usize, message: impl Into<String>) -> CallError {
        let end = (self.at + 1).min(self.text.len()).max(start);
        let end = (end..=self.text.len())
            .find(|&e| self.text.is_char_boundary(e))
            .unwrap_or(self.text.len());
        CallError {
            message: message.into(),
            range: start.min(end)..end,
        }
    }

    /// Skips whitespace.
    fn blanks(&mut self) {
        let rest = &self.text[self.at..];
        self.at += rest.len() - rest.trim_start().len();
    }

    /// Skips whitespace and comments.
    fn space(&mut self) -> Result<(), CallError> {
        loop {
            self.blanks();
            let rest = &self.text[self.at..];
            if rest.starts_with("//") {
                self.at += rest.find('\n').unwrap_or(rest.len());
            } else if let Some(inner) = rest.strip_prefix("/*") {
                let Some(end) = inner.find("*/") else {
                    return Err(self.error(self.at, "the comment /* is not closed"));
                };
                self.at += end + 4;
            } else {
                return Ok(());
            }
        }
    }

    /// The arguments of a call whose name starts at `start`, after `(`, and
    /// the closing `)`.
    fn arguments(&mut self, start: usize) -> Result<Samples, CallError> {
        let first_at = self.at;
        let first = self.value(0)?;
        self.space()?;
        let mut base = None;
        if self.eat(b',') {
            self.space()?;
            if self.peek() != Some(b')') {
                let at = self.at;
                match self.value(0)? {
                    Value::String(b) => base = Some(b),
                    _ => return Err(self.error(at, "samples' second argument is a base address")),
                }
                self.space()?;
                // Strudel's options, such as { tag: 'drums' }, change nothing here.
                if self.eat(b',') {
                    self.space()?;
                    if self.peek() != Some(b')') {
                        self.value(0)?;
                        self.space()?;
                        self.eat(b',');
                        self.space()?;
                    }
                }
            }
        }
        if !self.eat(b')') {
            return Err(self.error(self.at, "expected ) to close samples("));
        }
        let range = start..self.at;
        match first {
            Value::String(a) => address(&a, base, range),
            Value::Object(mut object) => {
                if let Some(b) = base {
                    object.entry("_base").or_insert(Value::String(b));
                }
                let map =
                    SampleMap::from_value(&Value::Object(object), "").map_err(|m| CallError {
                        message: m,
                        range: first_at..range.end,
                    })?;
                let relative = map
                    .sounds
                    .values()
                    .flatten()
                    .any(|f| !crate::is_url(&f.path));
                if map.base.is_empty() && relative {
                    return Err(CallError {
                        message: "give each sound a full https:// address, or the address the \
                                  paths start from as samples' second argument"
                            .into(),
                        range,
                    });
                }
                Ok(Samples::Map(map))
            }
            _ => Err(CallError {
                message: "samples() takes a pack's address in quotes or a sample map in { }".into(),
                range,
            }),
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, CallError> {
        if depth > MAX_DEPTH {
            return Err(self.error(self.at, "the sample map nests too deeply"));
        }
        let start = self.at;
        match self.peek() {
            Some(b'\'' | b'"' | b'`') => self.string().map(Value::String),
            Some(b'{') => {
                self.at += 1;
                let mut object = Map::new();
                loop {
                    self.space()?;
                    if self.eat(b'}') {
                        return Ok(Value::Object(object));
                    }
                    let key_at = self.at;
                    let key = match self.peek() {
                        Some(b'\'' | b'"' | b'`') => self.string()?,
                        Some(b) if is_word(b) => self.word(),
                        _ => return Err(self.error(key_at, "expected a sound name")),
                    };
                    self.space()?;
                    if !self.eat(b':') {
                        return Err(self.error(self.at, format!("expected : after {key}")));
                    }
                    self.space()?;
                    let value = self.value(depth + 1)?;
                    object.insert(key, value);
                    self.space()?;
                    if !self.eat(b',') {
                        self.space()?;
                        if self.eat(b'}') {
                            return Ok(Value::Object(object));
                        }
                        return Err(self.error(self.at, "expected , or } in the sample map"));
                    }
                }
            }
            Some(b'[') => {
                self.at += 1;
                let mut items = Vec::new();
                loop {
                    self.space()?;
                    if self.eat(b']') {
                        return Ok(Value::Array(items));
                    }
                    items.push(self.value(depth + 1)?);
                    self.space()?;
                    if !self.eat(b',') {
                        self.space()?;
                        if self.eat(b']') {
                            return Ok(Value::Array(items));
                        }
                        return Err(self.error(self.at, "expected , or ] in the list"));
                    }
                }
            }
            Some(b) if is_word(b) || b == b'-' => {
                let word = self.word();
                match word.as_str() {
                    "true" => Ok(Value::Bool(true)),
                    "false" => Ok(Value::Bool(false)),
                    "null" => Ok(Value::Null),
                    w => match w.parse::<f64>().ok().and_then(serde_json::Number::from_f64) {
                        Some(n) => Ok(Value::Number(n)),
                        None => Err(self.error(
                            start,
                            format!("{w} is not text in quotes; write the address in quotes"),
                        )),
                    },
                }
            }
            _ => Err(self.error(start, "expected text in quotes, a list or a sample map")),
        }
    }

    /// A name or number, unquoted.
    fn word(&mut self) -> String {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        while self.peek().is_some_and(is_word) {
            self.at += 1;
        }
        self.text[start..self.at].to_string()
    }

    /// Text in quotes, with its escapes.
    fn string(&mut self) -> Result<String, CallError> {
        let start = self.at;
        let quote = self.text.as_bytes()[self.at];
        self.at += 1;
        let mut out = String::new();
        loop {
            let Some(ch) = self.text[self.at..].chars().next() else {
                return Err(self.error(start, "the text in quotes is not closed"));
            };
            self.at += ch.len_utf8();
            match ch {
                c if c as u32 == quote as u32 => return Ok(out),
                '\n' if quote != b'`' => {
                    return Err(self.error(start, "the text in quotes is not closed"));
                }
                '$' if quote == b'`' && self.peek() == Some(b'{') => {
                    return Err(self.error(start, "${ } in a template is not supported"));
                }
                '\\' => {
                    let Some(next) = self.text[self.at..].chars().next() else {
                        return Err(self.error(start, "the text in quotes is not closed"));
                    };
                    self.at += next.len_utf8();
                    match next {
                        'n' => out.push('\n'),
                        't' => out.push('\t'),
                        'r' => out.push('\r'),
                        '0' => out.push('\0'),
                        'u' => {
                            let rest = &self.text[self.at..];
                            let (hex, used) = match rest.strip_prefix('{') {
                                Some(r) => {
                                    let end = r.find('}').unwrap_or(r.len());
                                    (&r[..end], end + 2)
                                }
                                None => (rest.get(..4).unwrap_or(rest), 4),
                            };
                            let Some(c) =
                                u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
                            else {
                                return Err(self.error(self.at, "a \\u escape needs a code point"));
                            };
                            self.at += used.min(rest.len());
                            out.push(c);
                        }
                        // A line ending after a backslash continues the text.
                        '\n' => {}
                        c => out.push(c),
                    }
                }
                c => out.push(c),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(text: &str) -> SamplesCall {
        let calls = find_samples(text).unwrap();
        assert_eq!(calls.len(), 1, "{calls:?}");
        calls.into_iter().next().unwrap()
    }

    #[test]
    fn calls_name_packs_by_address() {
        let text = "samples('github:tidalcycles/dirt-samples')\nbd sd";
        let call = one(text);
        assert_eq!(
            call.samples,
            Samples::Address("github:tidalcycles/dirt-samples".into())
        );
        assert_eq!(
            &text[call.range.clone()],
            "samples('github:tidalcycles/dirt-samples')"
        );
        let call = one("await samples(\"https://x.org/strudel.json\");\nbd");
        assert_eq!(call.range, 0..44);
        assert_eq!(
            call.samples,
            Samples::Address("https://x.org/strudel.json".into())
        );
        // A line that is only an address counts as a call.
        let text = "  github:tidalcycles/dirt-samples\nbd*4";
        let call = one(text);
        assert_eq!(call.range, 2..33);
        assert_eq!(
            address_in("samples('github:yaxu/clean-breaks')"),
            "github:yaxu/clean-breaks"
        );
        assert_eq!(address_in(" vcsl "), "vcsl");
    }

    #[test]
    fn calls_write_maps_in_javascript() {
        let text = "samples({\n  'slap': 'https://cdn.example.org/495416-lq.mp3',\n  \
                    whirl: \"https://cdn.example.org/495313-lq.mp3\", // a comment\n  \
                    hits: ['a.wav', 'b.wav'],\n}, 'https://example.org/kit/')\nslap whirl";
        let call = one(text);
        let Samples::Map(map) = &call.samples else {
            panic!("{call:?}");
        };
        assert_eq!(map.base, "https://example.org/kit/");
        assert_eq!(
            map.sounds.keys().collect::<Vec<_>>(),
            ["hits", "slap", "whirl"]
        );
        assert_eq!(
            map.locate(&map.sounds["hits"][1]),
            "https://example.org/kit/b.wav"
        );
        assert_eq!(
            map.locate(&map.sounds["slap"][0]),
            "https://cdn.example.org/495416-lq.mp3"
        );
        assert_eq!(strip_calls(text, std::slice::from_ref(&call)), "slap whirl");
        let blank = blank_calls(text, &[call]);
        assert_eq!(blank.len(), text.len());
        assert_eq!(blank.trim(), "slap whirl");
        assert_eq!(blank.lines().count(), text.lines().count());
    }

    #[test]
    fn mini_notation_is_left_alone() {
        for text in ["samples(3,8) bd", "bd sd", "mysamples('x')", "samples hh"] {
            assert!(find_samples(text).unwrap().is_empty(), "{text}");
        }
        assert_eq!(strip_calls("bd sd", &[]), "bd sd");
    }

    #[test]
    fn several_calls_and_the_lines_between() {
        let text = "samples('github:a/b')\n\nbd sd\nsamples({x: 'https://e.org/x.wav'}) hh";
        let calls = find_samples(text).unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(strip_calls(text, &calls), "bd sd\n hh");
    }

    #[test]
    fn bad_calls_say_where() {
        for (text, wants) in [
            ("samples('github:a/b'\nbd", "expected )"),
            ("samples('github:a/b)", "not closed"),
            ("samples({ bd: 'bd.wav' })", "full https://"),
            ("samples('shabda:bd,sd')", "shabda: packs are not supported"),
            ("samples({ bd: https })", "not text in quotes"),
            (
                "samples('github:a/b', 'https://base/')",
                "base address goes with",
            ),
            ("samples(`${x}`)", "not supported"),
        ] {
            let e = find_samples(text).unwrap_err();
            assert!(e.message.contains(wants), "{text}: {e}");
            assert!(e.range.end <= text.len() && e.range.start <= e.range.end);
        }
        let deep = format!("samples({{a: {}}})", "[".repeat(40));
        assert!(find_samples(&deep).unwrap_err().message.contains("nests"));
    }
}
