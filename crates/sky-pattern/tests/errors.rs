// SPDX-License-Identifier: GPL-3.0-or-later
//! Parse errors point at the exact offending text.

use sky_pattern::{ParseError, parse};

fn err(src: &str) -> ParseError {
    match parse(src) {
        Ok(_) => panic!("{src:?} should not parse"),
        Err(e) => e,
    }
}

/// Asserts the error's message, byte offset and length; the column follows
/// from the offset for single-line input.
#[track_caller]
fn check(src: &str, offset: usize, len: usize, message: &str) {
    let e = err(src);
    assert_eq!(
        (e.message.as_str(), e.offset, e.len),
        (message, offset, len),
        "{src:?}"
    );
    assert_eq!(e.column, src[..offset].chars().count() + 1, "{src:?}");
}

#[test]
fn display_includes_the_column() {
    assert_eq!(
        err("bd [sd").to_string(),
        "unclosed '[' opened here at column 4"
    );
}

#[test]
fn unbalanced_brackets() {
    check("[bd sd", 0, 1, "unclosed '[' opened here");
    check("bd [sd [hh]", 3, 1, "unclosed '[' opened here");
    check("<a b", 0, 1, "unclosed '<' opened here");
    check("bd sd]", 5, 1, "']' has no matching '['");
    check("bd>", 2, 1, "'>' has no matching '<'");
    check("bd)", 2, 1, "')' has no matching '('");
    check(
        "[bd>",
        3,
        1,
        "expected ']' to close the '[' at column 1, found '>'",
    );
    check(
        "a <b c]",
        6,
        1,
        "expected '>' to close the '<' at column 3, found ']'",
    );
}

#[test]
fn bad_speed_factors() {
    check(
        "bd*",
        2,
        1,
        "expected a number after '*', found the end of the pattern",
    );
    check("bd*x", 3, 1, "expected a number after '*', found 'x'");
    check("bd*[2]", 3, 1, "expected a number after '*', found '['");
    check("bd*0", 3, 1, "a speed factor must be greater than 0");
    check("bd*0.0", 3, 3, "a speed factor must be greater than 0");
    check("bd/0", 3, 1, "a slow-down factor must be greater than 0");
    check(
        "bd/",
        2,
        1,
        "expected a number after '/', found the end of the pattern",
    );
    check(
        "bd*1.23456",
        3,
        7,
        "a speed factor can have at most 4 decimal places",
    );
    check("bd*20000", 3, 5, "a speed factor can be at most 10000");
    check(
        "bd*99999999999999999999",
        3,
        20,
        "a speed factor can be at most 10000",
    );
    check("bd@0", 3, 1, "a weight must be greater than 0");
    check(
        "bd@",
        2,
        1,
        "expected a number after '@', found the end of the pattern",
    );
}

#[test]
fn bad_numbers() {
    check("bd*1.", 4, 1, "expected digits after the decimal point");
    check("3bd", 0, 3, "invalid number '3bd'");
    check("bd 1.2.3", 3, 5, "invalid number '1.2.3'");
}

#[test]
fn bad_euclid() {
    check("bd(3,0)", 5, 1, "a Euclidean rhythm needs at least 1 step");
    check("bd(9,8)", 3, 1, "can't fit 9 onsets into 8 steps");
    check(
        "bd(3,2000)",
        5,
        4,
        "a Euclidean rhythm can have at most 1024 steps",
    );
    check("bd(3,8", 2, 1, "unclosed '(' opened here");
    check("bd(3,", 2, 1, "unclosed '(' opened here");
    check(
        "bd(3 8)",
        5,
        1,
        "expected ',' and the number of steps, as in bd(3,8), found '8'",
    );
    check(
        "bd(3,8]",
        6,
        1,
        "expected ')' to close the Euclidean rhythm, found ']'",
    );
    check(
        "bd(x,8)",
        3,
        1,
        "expected the number of onsets, as in bd(3,8,2), found 'x'",
    );
    check(
        "bd(3,8,)",
        7,
        1,
        "expected the rotation, as in bd(3,8,2), found ')'",
    );
    check(
        "bd(1.5,8)",
        3,
        3,
        "the number of onsets must be a whole number",
    );
    check(
        "bd(3,8.5)",
        5,
        3,
        "the number of steps must be a whole number",
    );
    check("(3,8)", 0, 1, "'(' must follow a step");
}

#[test]
fn unknown_characters() {
    check("bd $ sd", 3, 1, "unexpected character '$'");
    check("bd . sd", 3, 1, "unexpected character '.'");
    check("bd ☃", 3, 3, "unexpected character '☃'");
    check("{bd sd}", 0, 1, "polymeter braces '{ }' are not supported");
    check(
        "bd -1",
        3,
        1,
        "unexpected '-'; negative numbers are only allowed for a Euclidean rotation",
    );
}

#[test]
fn modifiers_without_a_step() {
    check("*2", 0, 1, "'*' must follow a step");
    check("bd [/2]", 4, 1, "'/' must follow a step");
    check("?bd", 0, 1, "'?' must follow a step");
    check("@2", 0, 1, "'@' must follow a step");
    check("_ bd", 0, 1, "'_' must follow a step");
    check("[_]", 1, 1, "'_' must follow a step");
    check("! bd", 0, 1, "'!' must follow a step");
    check("bd, _", 4, 1, "'_' must follow a step");
}

#[test]
fn bad_repeats() {
    check("bd!0", 3, 1, "a repeat count must be at least 1");
    check("bd!1.5", 3, 3, "the repeat count must be a whole number");
    check("bd!2000", 2, 5, "a step can be repeated at most 1024 times");
    check(
        "bd!1000!100",
        7,
        1,
        "a step can be repeated at most 1024 times",
    );
}

#[test]
fn bad_probabilities() {
    check("bd?2", 3, 1, "a probability must be between 0 and 1");
    check("bd?1.5", 3, 3, "a probability must be between 0 and 1");
}

#[test]
fn bad_atoms() {
    check(
        "128",
        0,
        3,
        "note number 128 is outside the MIDI range 0-127",
    );
    check(
        "bd 999",
        3,
        3,
        "note number 999 is outside the MIDI range 0-127",
    );
    check(
        "60.5",
        0,
        4,
        "note numbers must be whole numbers; use '*' or '/' to change speed",
    );
    check("g#9", 0, 3, "note 'g#9' is outside the MIDI range 0-127");
    check("c10", 0, 3, "note 'c10' is outside the MIDI range 0-127");
    check("c4:2", 2, 2, "a note can't have a sample index");
    check("60:1", 2, 1, "a note number can't have a sample index");
    check("bd#", 2, 1, "'#' is only allowed in note names such as f#3");
    check("bd:", 2, 1, "expected a sample index after ':'");
    check("bd:x", 3, 1, "expected a sample index after ':'");
    check(
        "bd:3x",
        3,
        2,
        "invalid sample index '3x'; it must be a whole number",
    );
    check("bd:99999999999", 3, 11, "sample index is too large");
    check(
        "[bd]:2",
        4,
        1,
        "':' must directly follow a sound name, as in sd:3",
    );
}

#[test]
fn empty_layers_and_choices() {
    check("bd,", 2, 1, "expected steps after ','");
    check(", bd", 0, 1, "expected steps before ','");
    check("[bd, ]", 3, 1, "expected steps after ','");
    check("bd | ", 3, 1, "expected steps after '|'");
    check("| bd", 0, 1, "expected steps before '|'");
    check("<a,,b>", 2, 1, "expected steps after ','");
}

#[test]
fn too_many_events_per_cycle() {
    let dense = "this plays more than 65536 events per cycle";
    check("bd*9999*9999", 7, 5, dense);
    check("bd*256*257", 6, 4, dense);
    check("[bd!1024]!65", 0, 12, dense);
    check("bd*256*256, bd*256*256", 0, 22, dense);
    assert!(parse("bd*256*256").is_ok());
    assert!(parse("[bd!1024]!64").is_ok());
}

#[test]
fn too_finely_divided() {
    let fine = "this divides time too finely to keep exact timing; \
                use fewer nested decimal speeds or weights";
    check("[bd*1.2345]*1.2345*1.2345*1.2345", 18, 7, fine);
    check("[[[bd*1.333]*1.333]*1.333]*1.333", 19, 6, fine);
    check("[a b c d e f g h i j k]*9999.9999", 23, 10, fine);
    assert!(parse("[bd*1.2345]*1.2345").is_ok());
    assert!(parse("a/9999/9999/9999/9999").is_ok());
}

#[test]
fn columns_count_characters_on_the_current_line() {
    let e = err("bd sd\nhh [");
    assert_eq!((e.offset, e.len, e.column), (9, 1, 4));
    // U+00A0 is whitespace but two bytes long.
    let e = err("\u{a0}[bd");
    assert_eq!((e.offset, e.column), (2, 2));
    let e = err("bd\r\n  *");
    assert_eq!((e.offset, e.column), (6, 3));
}

#[test]
fn errors_stay_inside_the_source() {
    for src in [
        "[", "<", "bd(", "bd*", "bd:", "bd(3,8,", "a b c ]", "bd!", "bd?",
    ] {
        if let Err(e) = parse(src) {
            assert!(e.offset + e.len <= src.len(), "{src:?}: {e:?}");
            assert!(e.len > 0, "{src:?}: {e:?}");
        }
    }
}
