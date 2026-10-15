// SPDX-License-Identifier: GPL-3.0-or-later
//! Number patterns: the notation with numbers as its terms.

use sky_pattern::{Time, Value, parse, parse_numbers};

fn values(src: &str, cycle: i64) -> Vec<f64> {
    parse_numbers(src)
        .unwrap_or_else(|e| panic!("{src:?} should parse: {e}"))
        .query(Time::from_integer(cycle), Time::from_integer(cycle + 1), 0)
        .into_iter()
        .map(|e| e.value.number().expect("a number"))
        .collect()
}

#[track_caller]
fn error(src: &str, message: &str) {
    match parse_numbers(src) {
        Ok(_) => panic!("{src:?} should not parse"),
        Err(e) => assert_eq!(e.message, message, "{src:?}"),
    }
}

#[test]
fn numbers_of_any_size_and_sign() {
    assert_eq!(values("100 60 80 60", 0), [100.0, 60.0, 80.0, 60.0]);
    assert_eq!(values("400 20000", 0), [400.0, 20000.0]);
    assert_eq!(values("-12 0.25 -0.5", 0), [-12.0, 0.25, -0.5]);
}

#[test]
fn numbers_are_exact() {
    let events =
        parse_numbers("0.1")
            .unwrap()
            .query(Time::from_integer(0), Time::from_integer(1), 0);
    assert_eq!(events[0].value, Value::Number(Time::new(1, 10)));
}

#[test]
fn the_notation_works_on_numbers() {
    assert_eq!(values("<400 800 1600>", 0), [400.0]);
    assert_eq!(values("<400 800 1600>", 2), [1600.0]);
    assert_eq!(values("[1 2]*2 ~ 3", 0), [1.0, 2.0, 1.0, 2.0, 3.0]);
    assert_eq!(values("5(3,8)", 0), [5.0, 5.0, 5.0]);
    assert_eq!(values("1 -1!2", 0), [1.0, -1.0, -1.0]);
    assert_eq!(values("1, -2", 0).len(), 2);
}

#[test]
fn a_negative_number_spans_its_sign() {
    let events =
        parse_numbers("4 -12")
            .unwrap()
            .query(Time::from_integer(0), Time::from_integer(1), 0);
    assert_eq!((events[1].source.start, events[1].source.end), (2, 5));
}

#[test]
fn names_and_loose_signs_are_errors() {
    error(
        "100 bd",
        "expected a number, found 'bd'; this pattern takes numbers",
    );
    error(
        "c4",
        "expected a number, found 'c4'; this pattern takes numbers",
    );
    error("- 12", "'-' must come right before a number, as in -12");
    error("1 -", "'-' must come right before a number, as in -12");
    error("1:2", "a number can't have a sample index");
    error("0.1234567", "a number can have at most 6 decimal places");
    error(
        "2000000000",
        "a number can be at most 1000000000 either side of zero",
    );
}

#[test]
fn note_patterns_still_refuse_signs_and_numbers_still_play_notes() {
    assert!(parse("-12").is_err());
    let events = parse("60")
        .unwrap()
        .query(Time::from_integer(0), Time::from_integer(1), 0);
    assert_eq!(events[0].value, Value::Note(60));
    assert_eq!(events[0].value.number(), None);
}
