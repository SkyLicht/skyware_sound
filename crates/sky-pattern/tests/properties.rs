// SPDX-License-Identifier: GPL-3.0-or-later
//! Property tests: queries of mini-notation and Strudel code can be split
//! anywhere, results are in range and sorted, and the parser never panics.

use proptest::prelude::*;
use sky_pattern::{Event, Time, parse};

fn atom() -> impl Strategy<Value = String> {
    prop::sample::select(vec!["bd", "sd:2", "hh", "c4", "e", "f#3", "60", "~", "cb"])
        .prop_map(str::to_string)
}

fn modifier() -> impl Strategy<Value = String> {
    prop::sample::select(vec![
        "", "", "", "*2", "*3", "/2", "*1.5", "/1.5", "?", "?0.3", "(3,8)", "(2,5,1)", "@2",
        "@0.5", "!", "!2", " _",
    ])
    .prop_map(str::to_string)
}

fn sequence(step: impl Strategy<Value = String> + Clone) -> impl Strategy<Value = String> + Clone {
    prop::collection::vec(step, 1..4).prop_map(|steps| steps.join(" "))
}

/// Patterns from a small grammar covering every operator.
fn pattern() -> impl Strategy<Value = String> {
    let leaf = (atom(), modifier()).prop_map(|(a, m)| format!("{a}{m}"));
    let step = leaf.prop_recursive(3, 32, 3, |inner| {
        let seq = sequence(inner);
        prop_oneof![
            (seq.clone(), modifier()).prop_map(|(s, m)| format!("[{s}]{m}")),
            (seq.clone(), modifier()).prop_map(|(s, m)| format!("<{s}>{m}")),
            (seq.clone(), seq.clone()).prop_map(|(a, b)| format!("[{a}, {b}]")),
            (seq.clone(), seq).prop_map(|(a, b)| format!("[{a} | {b}]")),
        ]
    });
    sequence(step)
}

/// Strudel code from the functions that change time, structure and values.
fn code() -> impl Strategy<Value = String> {
    let base = prop::sample::select(vec![
        "s(\"bd*2 [~ sd] <hh oh>\")",
        "note(\"c3 <e3 g3> [a3 c4]\")",
        "n(\"0 2 <4 7>\")",
        "n(sine.segment(4).range(0, 7))",
        "s(\"hh\").struct(\"x ~ x x\")",
        "cat(s(\"bd\"), s(\"sd sd\"))",
        "stack(s(\"bd(3,8)\"), note(\"c3\").slow(2))",
    ]);
    let method = prop::sample::select(vec![
        ".fast(2)",
        ".slow(1.5)",
        ".fast(\"<1 2>\")",
        ".fast(\"1 3\")",
        ".early(0.25)",
        ".late(1/3)",
        ".rev()",
        ".palindrome()",
        ".brak()",
        ".ply(2)",
        ".iter(3)",
        ".iterBack(2)",
        ".swingBy(1/3, 4)",
        ".every(3, rev)",
        ".lastOf(2, fast(2))",
        ".chunk(4, x => x.gain(0))",
        ".within(0, 0.5, x => x.fast(2))",
        ".inside(2, rev)",
        ".outside(2, rev)",
        ".sometimes(x => x.speed(2))",
        ".someCycles(x => x.gain(0.5))",
        ".degradeBy(0.3)",
        ".undegradeBy(0.3)",
        ".struct(\"x ~ x x\")",
        ".mask(\"1 0 1\")",
        ".euclid(3, 8)",
        ".euclidLegato(3, 8)",
        ".superimpose(x => x.late(0.125))",
        ".off(0.25, x => x.add(12))",
        ".echo(3, 1/8, 0.5)",
        ".add(\"<0 12>\")",
        ".gain(sine)",
        ".lpf(saw.range(200, 2000).slow(2))",
        ".legato(0.5)",
        ".hurry(2)",
        ".n(\"0 1\")",
        ".s(\"piano\")",
        ".bank(\"RolandTR909\")",
        ".scale(\"C:minor\")",
        ".jux(rev)",
        ".room(0.5)",
    ]);
    (base, prop::collection::vec(method, 0..5)).prop_map(|(b, ms)| format!("{b}{}", ms.concat()))
}

/// Pieces of Strudel code, put together at random.
fn code_token() -> impl Strategy<Value = String> {
    prop::sample::select(vec![
        "s(",
        "note(",
        "n(",
        "stack(",
        "cat(",
        "\"bd sd\"",
        "\"<0 12>\"",
        "\"x ~ x\"",
        "'c3'",
        ")",
        "(",
        ",",
        ".",
        "fast",
        "every",
        "rev",
        "sine",
        "segment",
        "add",
        "gain",
        "struct",
        "sometimes",
        "off",
        "x",
        "=>",
        "2",
        "1/3",
        "-",
        "+",
        "*",
        "[",
        "]",
        "$:",
        "_$:",
        "\n",
        "let ",
        "=",
        ";",
        " ",
        "//",
        "`",
        "\"",
        "await ",
        "samples('a')",
        "room",
    ])
    .prop_map(str::to_string)
}

/// Three ordered times between -2 and 6 cycles.
fn bounds() -> impl Strategy<Value = (Time, Time, Time)> {
    (
        -24i64..72,
        -24i64..72,
        -24i64..72,
        prop::sample::select(vec![1i64, 2, 3, 4, 6, 7, 12]),
    )
        .prop_map(|(a, b, c, d)| {
            let mut v = [a, b, c];
            v.sort();
            (Time::new(v[0], d), Time::new(v[1], d), Time::new(v[2], d))
        })
}

fn sorted(events: &[Event]) -> bool {
    events
        .windows(2)
        .all(|w| (w[0].begin, w[0].source.start) <= (w[1].begin, w[1].source.start))
}

proptest! {
    #[test]
    fn queries_split_cleanly(src in pattern(), (a, b, c) in bounds(), seed in any::<u64>()) {
        let p = parse(&src).map_err(|e| TestCaseError::fail(format!("{src:?}: {e}")))?;
        let whole = p.query(a, c, seed);
        let mut parts = p.query(a, b, seed);
        parts.extend(p.query(b, c, seed));
        prop_assert_eq!(&whole, &parts, "{:?}", src);
        prop_assert!(sorted(&whole), "{:?}", src);
        for e in &whole {
            prop_assert!(e.begin >= a && e.begin < c, "{:?}: {:?}", src, e);
            prop_assert!(e.begin < e.end, "{:?}: {:?}", src, e);
            prop_assert!(e.source.end <= src.len());
        }
        prop_assert_eq!(whole, p.query(a, c, seed));
    }

    #[test]
    fn code_queries_split_cleanly(src in code(), (a, b, c) in bounds(), seed in any::<u64>()) {
        let p = parse(&src).map_err(|e| TestCaseError::fail(format!("{src:?}: {e}")))?;
        let whole = p.query(a, c, seed);
        let mut parts = p.query(a, b, seed);
        parts.extend(p.query(b, c, seed));
        prop_assert_eq!(&whole, &parts, "{:?}", src);
        prop_assert!(sorted(&whole), "{:?}", src);
        for e in &whole {
            prop_assert!(e.begin >= a && e.begin < c, "{:?}: {:?}", src, e);
            prop_assert!(e.begin < e.end, "{:?}: {:?}", src, e);
            prop_assert!(e.source.end <= src.len());
        }
    }

    #[test]
    fn parser_never_panics(src in "[a-z0-9 ~_\\[\\]<>(),|*/!@?:#.\\-]{0,24}") {
        // Only parse: random text can ask for huge speed-ups such as a*99*99*99.
        if let Err(e) = parse(&src) {
            prop_assert!(e.offset + e.len <= src.len(), "{:?}: {:?}", src, e);
            prop_assert!(e.len > 0, "{:?}: {:?}", src, e);
            prop_assert!(e.column >= 1);
        }
    }

    #[test]
    fn code_parser_never_panics(parts in prop::collection::vec(code_token(), 0..14)) {
        let src = parts.concat();
        match parse(&src) {
            Ok(p) => {
                let events = p.query(Time::from_integer(0), Time::from_integer(2), 1);
                prop_assert!(sorted(&events), "{:?}", src);
            }
            Err(e) => {
                prop_assert!(e.offset + e.len <= src.len(), "{:?}: {:?}", src, e);
                prop_assert!(src.is_char_boundary(e.offset));
                prop_assert!(src.is_char_boundary(e.offset + e.len));
            }
        }
    }

    #[test]
    fn parser_never_panics_on_any_text(src in "\\PC{0,16}") {
        if let Err(e) = parse(&src) {
            prop_assert!(src.is_char_boundary(e.offset));
            prop_assert!(src.is_char_boundary(e.offset + e.len));
        }
    }
}
