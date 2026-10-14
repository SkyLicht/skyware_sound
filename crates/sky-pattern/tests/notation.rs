// SPDX-License-Identifier: GPL-3.0-or-later
//! Event times and values for each mini-notation feature.

use sky_pattern::{Event, Pattern, Span, Time, Value, drum_note, parse};

fn t(numer: i64, denom: i64) -> Time {
    Time::new(numer, denom)
}

fn int(n: i64) -> Time {
    Time::from_integer(n)
}

fn snd(name: &str) -> Value {
    Value::Sound {
        name: name.to_string(),
        index: 0,
    }
}

fn note(n: u8) -> Value {
    Value::Note(n)
}

fn pat(src: &str) -> Pattern {
    parse(src).unwrap_or_else(|e| panic!("{src:?} should parse: {e}"))
}

/// `(begin, end, value)` of every event with an onset in `[from, to)`, seed 0.
fn events(src: &str, from: Time, to: Time) -> Vec<(Time, Time, Value)> {
    pat(src)
        .query(from, to, 0)
        .into_iter()
        .map(|e| (e.begin, e.end, e.value))
        .collect()
}

fn cycle(src: &str, c: i64) -> Vec<(Time, Time, Value)> {
    events(src, int(c), int(c + 1))
}

fn values(src: &str, c: i64) -> Vec<Value> {
    cycle(src, c).into_iter().map(|(_, _, v)| v).collect()
}

fn begins(src: &str, from: Time, to: Time) -> Vec<Time> {
    events(src, from, to)
        .into_iter()
        .map(|(b, _, _)| b)
        .collect()
}

/// The one value each cycle plays, for cycles `0..n`.
fn per_cycle(src: &str, n: i64) -> Vec<Value> {
    (0..n)
        .map(|c| {
            let vals = values(src, c);
            assert_eq!(vals.len(), 1, "{src:?} cycle {c}: {vals:?}");
            vals[0].clone()
        })
        .collect()
}

// --- Sequences, rests and subsequences ---

#[test]
fn two_step_sequence() {
    assert_eq!(
        cycle("bd sd", 0),
        [(int(0), t(1, 2), snd("bd")), (t(1, 2), int(1), snd("sd"))]
    );
}

#[test]
fn three_steps_divide_the_cycle() {
    assert_eq!(
        begins("bd sd hh", int(0), int(1)),
        [int(0), t(1, 3), t(2, 3)]
    );
    assert_eq!(values("bd sd hh", 0), [snd("bd"), snd("sd"), snd("hh")]);
}

#[test]
fn every_cycle_repeats() {
    assert_eq!(
        cycle("bd sd", 5),
        [(int(5), t(11, 2), snd("bd")), (t(11, 2), int(6), snd("sd"))]
    );
}

#[test]
fn rests_take_a_step() {
    assert_eq!(
        cycle("bd ~ sd ~", 0),
        [(int(0), t(1, 4), snd("bd")), (t(1, 2), t(3, 4), snd("sd"))]
    );
}

#[test]
fn empty_and_silent_patterns() {
    for src in [
        "", "   ", "\n\t", "~", "~ ~ ~", "[]", "<>", "[~ ~]*4", "~(3,8)", "~?",
    ] {
        assert!(cycle(src, 0).is_empty(), "{src:?}");
        assert!(events(src, int(-3), int(9)).is_empty(), "{src:?}");
    }
}

#[test]
fn subsequence_fits_in_one_step() {
    assert_eq!(
        cycle("bd [sd sd]", 0),
        [
            (int(0), t(1, 2), snd("bd")),
            (t(1, 2), t(3, 4), snd("sd")),
            (t(3, 4), int(1), snd("sd")),
        ]
    );
}

#[test]
fn nested_subsequences() {
    assert_eq!(
        cycle("[a [b [c d]]]", 0),
        [
            (int(0), t(1, 2), note(57)),
            (t(1, 2), t(3, 4), note(59)),
            (t(3, 4), t(7, 8), note(48)),
            (t(7, 8), int(1), note(50)),
        ]
    );
}

#[test]
fn whitespace_and_newlines_separate_steps() {
    assert_eq!(cycle("bd\n  sd\thh", 0), cycle("bd sd hh", 0));
    assert_eq!(cycle(" [ bd  sd ] ", 0), cycle("bd sd", 0));
}

// --- Stacks ---

#[test]
fn top_level_stack() {
    let evs = cycle("bd*2, hh*4", 0);
    assert_eq!(evs.len(), 6);
    assert_eq!(
        evs.iter()
            .map(|(b, _, v)| (*b, v.clone()))
            .collect::<Vec<_>>(),
        [
            (int(0), snd("bd")),
            (int(0), snd("hh")),
            (t(1, 4), snd("hh")),
            (t(1, 2), snd("bd")),
            (t(1, 2), snd("hh")),
            (t(3, 4), snd("hh")),
        ]
    );
}

#[test]
fn chord() {
    assert_eq!(
        cycle("[c4,e4,g4]", 0),
        [
            (int(0), int(1), note(60)),
            (int(0), int(1), note(64)),
            (int(0), int(1), note(67)),
        ]
    );
}

#[test]
fn chord_inside_a_sequence() {
    assert_eq!(
        cycle("c4 [e4,g4]", 0),
        [
            (int(0), t(1, 2), note(60)),
            (t(1, 2), int(1), note(64)),
            (t(1, 2), int(1), note(67)),
        ]
    );
}

#[test]
fn stack_layers_divide_independently() {
    assert_eq!(
        begins("[a b, c d e]", int(0), int(1)),
        [int(0), int(0), t(1, 3), t(1, 2), t(2, 3)]
    );
}

// --- Alternation ---

#[test]
fn alternation_plays_one_per_cycle() {
    assert_eq!(
        per_cycle("<a b>", 4),
        [note(57), note(59), note(57), note(59)]
    );
    assert_eq!(cycle("<a b>", 1), [(int(1), int(2), note(59))]);
}

#[test]
fn alternation_in_negative_cycles() {
    assert_eq!(values("<a b c>", -1), [note(48)]);
    assert_eq!(values("<a b c>", -3), [note(57)]);
}

#[test]
fn alternation_of_subsequences() {
    assert_eq!(
        cycle("<[a b] c>", 0),
        [(int(0), t(1, 2), note(57)), (t(1, 2), int(1), note(59))]
    );
    assert_eq!(cycle("<[a b] c>", 1), [(int(1), int(2), note(48))]);
    assert_eq!(
        cycle("<[a b] c>", 2),
        cycle("<[a b] c>", 0)
            .into_iter()
            .map(|(b, e, v)| (b + 2, e + 2, v))
            .collect::<Vec<_>>()
    );
}

#[test]
fn sped_up_alternation_advances_each_repetition() {
    for c in 0..3 {
        let base = int(c);
        assert_eq!(
            cycle("<a b>*2", c),
            [
                (base, base + t(1, 2), note(57)),
                (base + t(1, 2), base + 1, note(59)),
            ]
        );
    }
}

#[test]
fn alternation_inside_a_sequence_advances_per_cycle() {
    assert_eq!(values("<a b> c", 0), [note(57), note(48)]);
    assert_eq!(values("<a b> c", 1), [note(59), note(48)]);
    assert_eq!(
        values("[<a b> c]*2", 0),
        [note(57), note(48), note(59), note(48)]
    );
}

#[test]
fn nested_alternation_keeps_its_own_count() {
    assert_eq!(
        per_cycle("<a <b c>>", 4),
        [note(57), note(59), note(57), note(48)]
    );
}

#[test]
fn alternation_layers_cycle_independently() {
    let pairs: Vec<Vec<Value>> = (0..4).map(|c| values("<a b, c d e>", c)).collect();
    assert_eq!(
        pairs,
        [
            vec![note(57), note(48)],
            vec![note(59), note(50)],
            vec![note(57), note(52)],
            vec![note(59), note(48)],
        ]
    );
}

#[test]
fn alternation_with_replication_and_weights() {
    assert_eq!(per_cycle("<a!2 b>", 3), [note(57), note(57), note(59)]);
    // A weight of 3 makes the step last three cycles.
    let evs = events("<a@3 b>", int(0), int(8));
    assert_eq!(
        evs,
        [
            (int(0), int(3), note(57)),
            (int(3), int(4), note(59)),
            (int(4), int(7), note(57)),
            (int(7), int(8), note(59)),
        ]
    );
}

// --- Speed ---

#[test]
fn fast() {
    assert_eq!(
        cycle("bd*2", 0),
        [(int(0), t(1, 2), snd("bd")), (t(1, 2), int(1), snd("bd"))]
    );
    assert_eq!(
        begins("hh*4", int(0), int(1)),
        [int(0), t(1, 4), t(1, 2), t(3, 4)]
    );
}

#[test]
fn fast_by_a_decimal() {
    assert_eq!(
        events("bd*1.5", int(0), int(2)),
        [
            (int(0), t(2, 3), snd("bd")),
            (t(2, 3), t(4, 3), snd("bd")),
            (t(4, 3), int(2), snd("bd")),
        ]
    );
}

#[test]
fn slow_sounds_every_other_cycle() {
    assert_eq!(
        events("bd/2", int(0), int(2)),
        [(int(0), int(2), snd("bd"))]
    );
    assert_eq!(cycle("bd/2", 0), [(int(0), int(2), snd("bd"))]);
    assert!(cycle("bd/2", 1).is_empty());
    assert_eq!(cycle("bd/2", 2), [(int(2), int(4), snd("bd"))]);
}

#[test]
fn slow_step_in_a_sequence() {
    assert_eq!(
        cycle("bd/2 sd", 0),
        [(int(0), int(1), snd("bd")), (t(1, 2), int(1), snd("sd"))]
    );
    assert_eq!(cycle("bd/2 sd", 1), [(t(3, 2), int(2), snd("sd"))]);
}

#[test]
fn slow_by_a_decimal() {
    assert_eq!(begins("bd/1.5", int(0), int(3)), [int(0), t(3, 2)]);
}

#[test]
fn fast_subsequence() {
    assert_eq!(
        begins("[bd sd]*2", int(0), int(1)),
        [int(0), t(1, 4), t(1, 2), t(3, 4)]
    );
    assert_eq!(
        values("[bd sd]*2", 0),
        [snd("bd"), snd("sd"), snd("bd"), snd("sd")]
    );
}

#[test]
fn fast_then_slow() {
    assert_eq!(cycle("bd*4/2", 0), cycle("bd*2", 0));
}

// --- Replication and elongation ---

#[test]
fn replicate() {
    assert_eq!(
        cycle("a!3 b", 0),
        [
            (int(0), t(1, 4), note(57)),
            (t(1, 4), t(1, 2), note(57)),
            (t(1, 2), t(3, 4), note(57)),
            (t(3, 4), int(1), note(59)),
        ]
    );
}

#[test]
fn bare_bang_repeats_the_previous_step() {
    assert_eq!(cycle("a! b", 0), cycle("a a b", 0));
    assert_eq!(cycle("a ! b", 0), cycle("a a b", 0));
    assert_eq!(cycle("a ! ! b", 0), cycle("a a a b", 0));
    assert_eq!(cycle("a!!", 0), cycle("a a a", 0));
    assert_eq!(cycle("[a b]!", 0), cycle("[a b] [a b]", 0));
    assert_eq!(cycle("a !3 b", 0), cycle("a a a b", 0));
}

#[test]
fn bang_count_must_touch_the_bang() {
    // `a! 3` is `a a` followed by the note 3.
    assert_eq!(values("a! 3", 0), [note(57), note(57), note(3)]);
}

#[test]
fn elongate_with_at() {
    assert_eq!(
        cycle("a@3 b", 0),
        [(int(0), t(3, 4), note(57)), (t(3, 4), int(1), note(59))]
    );
    assert_eq!(
        cycle("a@1.5 b", 0),
        [(int(0), t(3, 5), note(57)), (t(3, 5), int(1), note(59))]
    );
}

#[test]
fn elongate_with_underscore() {
    assert_eq!(
        cycle("a _ b", 0),
        [(int(0), t(2, 3), note(57)), (t(2, 3), int(1), note(59))]
    );
    assert_eq!(cycle("a _ b", 0), cycle("a@2 b", 0));
    assert_eq!(cycle("a _ _ b", 0), cycle("a@3 b", 0));
    assert_eq!(cycle("a@2 _ b", 0), cycle("a@3 b", 0));
}

#[test]
fn replicate_after_underscore() {
    assert_eq!(cycle("a _ ! b", 0), cycle("a@2 a@2 b", 0));
}

#[test]
fn replicated_steps_keep_their_weight() {
    assert_eq!(cycle("a@2!2 b", 0), cycle("a@2 a@2 b", 0));
    assert_eq!(cycle("a!2@2 b", 0), cycle("a@2 a@2 b", 0));
}

// --- Euclidean rhythms ---

#[test]
fn euclid() {
    assert_eq!(
        cycle("bd(3,8)", 0),
        [
            (int(0), t(1, 8), snd("bd")),
            (t(3, 8), t(4, 8), snd("bd")),
            (t(6, 8), t(7, 8), snd("bd")),
        ]
    );
}

#[test]
fn euclid_rotated() {
    assert_eq!(
        begins("bd(3,8,2)", int(0), int(1)),
        [t(1, 8), t(4, 8), t(6, 8)]
    );
    assert_eq!(
        begins("bd(3,8,-1)", int(0), int(1)),
        [t(1, 8), t(4, 8), t(7, 8)]
    );
    assert_eq!(cycle("bd(3,8,8)", 0), cycle("bd(3,8)", 0));
}

#[test]
fn euclid_patterns() {
    assert_eq!(
        begins("bd(5,8)", int(0), int(1)),
        [int(0), t(2, 8), t(3, 8), t(5, 8), t(6, 8)]
    );
    assert_eq!(
        begins("bd(4,4)", int(0), int(1)),
        begins("bd*4", int(0), int(1))
    );
    assert!(cycle("bd(0,8)", 0).is_empty());
    assert_eq!(cycle("bd(1,1)", 0), cycle("bd", 0));
}

#[test]
fn euclid_with_whitespace() {
    assert_eq!(cycle("bd( 3 , 8 , 2 )", 0), cycle("bd(3,8,2)", 0));
    assert_eq!(cycle("bd (3,8)", 0), cycle("bd(3,8)", 0));
}

#[test]
fn euclid_of_an_alternation() {
    assert_eq!(values("<bd sd>(3,8)", 0), [snd("bd"), snd("bd"), snd("bd")]);
    assert_eq!(values("<bd sd>(3,8)", 1), [snd("sd"), snd("sd"), snd("sd")]);
}

#[test]
fn euclid_of_a_subsequence() {
    assert_eq!(
        begins("[bd sd](3,8)", int(0), int(1)),
        [int(0), t(1, 16), t(3, 8), t(7, 16), t(3, 4), t(13, 16)]
    );
}

#[test]
fn euclid_in_a_sequence() {
    assert_eq!(
        begins("bd(3,8) sd", int(0), int(1)),
        [int(0), t(3, 16), t(6, 16), t(1, 2)]
    );
    assert_eq!(cycle("bd(3,8)*2", 0).len(), 6);
}

// --- Randomness ---

fn count_events(src: &str, cycles: i64, seed: u64) -> usize {
    pat(src).query(int(0), int(cycles), seed).len()
}

#[test]
fn degrade_is_stable_for_a_seed() {
    let p = pat("bd?");
    let first = p.query(int(0), int(64), 42);
    assert_eq!(first, p.query(int(0), int(64), 42));
    assert_eq!(first, pat("bd?").query(int(0), int(64), 42));
    assert_ne!(first, p.query(int(0), int(64), 43));
}

#[test]
fn degrade_drops_about_half() {
    for seed in [0, 1, 7, 12345] {
        let kept = count_events("bd?", 1000, seed);
        assert!((400..=600).contains(&kept), "seed {seed}: kept {kept}");
    }
}

#[test]
fn degrade_with_probability() {
    let kept = count_events("bd?0.3", 1000, 3);
    assert!((600..=800).contains(&kept), "kept {kept}");
    assert_eq!(count_events("bd?0", 100, 3), 100);
    assert_eq!(count_events("bd?1", 100, 3), 0);
    assert_eq!(count_events("bd?1.0", 100, 3), 0);
}

#[test]
fn degrade_decides_each_event_separately() {
    // With `hh*8?` every hit gets its own decision, so cycles are mixed.
    let p = pat("hh*8?");
    let counts: Vec<usize> = (0..50)
        .map(|c| p.query(int(c), int(c + 1), 9).len())
        .collect();
    assert!(counts.iter().any(|&n| n > 0 && n < 8), "{counts:?}");
    let total: usize = counts.iter().sum();
    assert!((120..=280).contains(&total), "{total}");
    // Replicated copies are decided separately too.
    let p = pat("bd?!2");
    let mixed = (0..50).any(|c| p.query(int(c), int(c + 1), 9).len() == 1);
    assert!(mixed);
}

#[test]
fn degrade_after_fast() {
    let p = pat("bd*2?");
    let total = p.query(int(0), int(500), 5).len();
    assert!((400..=600).contains(&total), "{total}");
    for e in p.query(int(0), int(10), 5) {
        assert!(e.begin.denom() <= &2);
    }
}

#[test]
fn choice_is_deterministic_and_covers_all_options() {
    let p = pat("bd|sd|hh");
    let a = p.query(int(0), int(300), 11);
    assert_eq!(a, p.query(int(0), int(300), 11));
    assert_eq!(a.len(), 300);
    let count = |name: &str| a.iter().filter(|e| e.value == snd(name)).count();
    for name in ["bd", "sd", "hh"] {
        assert!((60..=140).contains(&count(name)), "{name}: {}", count(name));
    }
    for (c, e) in a.iter().enumerate() {
        assert_eq!(e.begin, int(c as i64));
        assert_eq!(e.end, int(c as i64 + 1));
    }
    assert_ne!(a, p.query(int(0), int(300), 12));
}

#[test]
fn choice_between_sequences() {
    let p = pat("[bd sd | hh hh hh]");
    let mut seen = [false; 2];
    for c in 0..64 {
        let evs = p.query(int(c), int(c + 1), 1);
        match evs.len() {
            2 => {
                seen[0] = true;
                assert_eq!(evs[0].value, snd("bd"));
                assert_eq!(evs[1].begin, int(c) + t(1, 2));
            }
            3 => {
                seen[1] = true;
                assert!(evs.iter().all(|e| e.value == snd("hh")));
            }
            n => panic!("cycle {c}: {n} events"),
        }
    }
    assert_eq!(seen, [true, true]);
}

#[test]
fn choice_binds_looser_than_stack() {
    // Either the stack `bd*2, hh` or `sd`.
    let p = pat("bd*2, hh | sd");
    let mut kinds = std::collections::BTreeSet::new();
    for c in 0..64 {
        kinds.insert(p.query(int(c), int(c + 1), 2).len());
    }
    assert_eq!(kinds.into_iter().collect::<Vec<_>>(), [1, 3]);
}

#[test]
fn choice_in_a_step_changes_each_repetition() {
    let p = pat("[bd|sd]*4");
    let evs = p.query(int(0), int(16), 4);
    assert_eq!(evs.len(), 64);
    let bd = evs.iter().filter(|e| e.value == snd("bd")).count();
    assert!((16..=48).contains(&bd), "{bd}");
}

#[test]
fn random_results_do_not_depend_on_query_splits() {
    for src in ["bd? sd*4?0.7", "[bd|sd|hh]*3", "<a|b c> [d? e]!2", "hh*16?"] {
        let p = pat(src);
        let whole = p.query(int(0), int(8), 77);
        let mut parts = Vec::new();
        for i in 0..64 {
            parts.extend(p.query(t(i, 8), t(i + 1, 8), 77));
        }
        assert_eq!(whole, parts, "{src}");
    }
}

// --- Atoms ---

#[test]
fn sound_index() {
    assert_eq!(
        values("sd:3 bd:0 hh", 0),
        [
            Value::Sound {
                name: "sd".into(),
                index: 3
            },
            snd("bd"),
            snd("hh"),
        ]
    );
}

#[test]
fn numbers_are_notes() {
    assert_eq!(values("60 64 67", 0), [note(60), note(64), note(67)]);
    assert_eq!(values("0 127", 0), [note(0), note(127)]);
}

#[test]
fn note_names() {
    assert_eq!(values("c d e", 0), [note(48), note(50), note(52)]);
    assert_eq!(values("c4 e4 g4", 0), [note(60), note(64), note(67)]);
    assert_eq!(
        values("f#3 eb5 cs4 bb", 0),
        [note(54), note(75), note(61), note(58)]
    );
    assert_eq!(values("C4 A", 0), [note(60), note(57)]);
    assert_eq!(values("a b", 0), [note(57), note(59)]);
}

#[test]
fn other_names_are_sounds() {
    assert_eq!(
        values("piano bass808 h c4x my_kit:2", 0),
        [
            snd("piano"),
            snd("bass808"),
            snd("h"),
            snd("c4x"),
            Value::Sound {
                name: "my_kit".into(),
                index: 2
            },
        ]
    );
}

#[test]
fn drum_names_win_over_note_names() {
    assert_eq!(values("cb cb3", 0), [snd("cb"), note(47)]);
}

#[test]
fn midi_notes() {
    assert_eq!(note(60).midi_note(), Some(60));
    assert_eq!(snd("bd").midi_note(), Some(36));
    assert_eq!(snd("piano").midi_note(), None);
    let sd3 = Value::Sound {
        name: "sd".into(),
        index: 3,
    };
    assert_eq!(sd3.midi_note(), Some(38));
    let notes: Vec<Option<u8>> = values("bd sd hh oh cp", 0)
        .iter()
        .map(Value::midi_note)
        .collect();
    assert_eq!(notes, [Some(36), Some(38), Some(42), Some(46), Some(39)]);
    assert_eq!(drum_note("snare"), Some(38));
}

// --- Stacked modifiers ---

#[test]
fn modifiers_stack() {
    assert_eq!(
        cycle("hh*4@2 sd", 0),
        [
            (int(0), t(1, 6), snd("hh")),
            (t(1, 6), t(1, 3), snd("hh")),
            (t(1, 3), t(1, 2), snd("hh")),
            (t(1, 2), t(2, 3), snd("hh")),
            (t(2, 3), int(1), snd("sd")),
        ]
    );
    assert_eq!(cycle("bd*2?0", 0), cycle("bd*2", 0));
    assert_eq!(cycle("[bd sd]*2!2", 0), cycle("[bd sd]*2 [bd sd]*2", 0));
    assert_eq!(cycle("bd(3,8)?0", 0), cycle("bd(3,8)", 0));
}

// --- Query ranges ---

#[test]
fn query_spanning_cycles_with_fractional_bounds() {
    assert_eq!(
        events("bd sd", t(1, 2), t(5, 2)),
        [
            (t(1, 2), int(1), snd("sd")),
            (int(1), t(3, 2), snd("bd")),
            (t(3, 2), int(2), snd("sd")),
            (int(2), t(5, 2), snd("bd")),
        ]
    );
}

#[test]
fn only_onsets_count() {
    // bd starts at 0, before the range, so it isn't returned.
    assert!(events("bd sd", t(1, 4), t(1, 2)).is_empty());
    assert_eq!(begins("bd sd", t(1, 4), t(3, 4)), [t(1, 2)]);
    // The end is exclusive.
    assert_eq!(begins("bd sd", int(0), t(1, 2)), [int(0)]);
}

#[test]
fn empty_or_reversed_ranges() {
    assert!(events("bd*4", int(1), int(1)).is_empty());
    assert!(events("bd*4", int(2), int(1)).is_empty());
}

#[test]
fn negative_times() {
    assert_eq!(
        events("bd sd", int(-1), int(0)),
        [
            (int(-1), t(-1, 2), snd("bd")),
            (t(-1, 2), int(0), snd("sd"))
        ]
    );
}

#[test]
fn far_away_cycles() {
    let p = pat("[a b c]*7 <d e f g>/3 [h i j k l]@1.5 a(5,8,3)?");
    let c = 1_000_000;
    let evs = p.query(int(c), int(c + 1), 3);
    assert!(!evs.is_empty());
    assert!(
        evs.iter()
            .all(|e| e.begin >= int(c) && e.begin < int(c + 1))
    );
}

#[test]
fn decimal_speeds_stay_exact_far_from_zero() {
    // Nested decimal factors and weights make large denominators; queries
    // near the ±2^31-cycle limit must still be exact and must not overflow.
    for src in [
        "[bd*1.333]*1.333 sd",
        "[bd*1.2345]*1.2345",
        "[a b c d e f g]*1.75 [a@1.25 b@0.75 c]*0.6667",
        "<[a b]*1.5 [c d e]/2.5 [f g h i]@1.75>(5,8)",
        "[a b c d e f g h i j k]*999.9999",
        "a/9999/9999/9999/9999 b",
    ] {
        let p = pat(src);
        for start in [0, 1_000_000, (1 << 31) - 2, -(1 << 31)] {
            let (from, to) = (int(start), int(start + 2));
            let whole = p.query(from, to, 5);
            assert!(!whole.is_empty(), "{src} at {start}");
            let mut parts = p.query(from, t(2 * start + 1, 2), 5);
            parts.extend(p.query(t(2 * start + 1, 2), to, 5));
            assert_eq!(whole, parts, "{src} at {start}");
            for e in &whole {
                assert!(e.begin >= from && e.begin < to && e.begin < e.end);
            }
        }
    }
}

#[test]
fn queries_are_clamped_to_two_to_the_31_cycles() {
    let p = pat("bd");
    let limit = 1i64 << 31;
    assert_eq!(
        begins("bd", int(limit - 1), int(i64::MAX)),
        [int(limit - 1)]
    );
    assert!(p.query(int(limit), int(limit + 5), 0).is_empty());
    assert_eq!(p.query(int(-limit - 5), int(-limit + 1), 0).len(), 1);
    assert!(p.query_cycles(1e300, f64::INFINITY, 0).is_empty());
}

#[test]
fn small_queries_add_up_to_the_cycle() {
    let p = pat("bd*3 [sd hh]*2 <cp oh>(3,8)");
    let whole = p.query(int(0), int(4), 0);
    let mut parts = Vec::new();
    for i in 0..(4 * 96) {
        parts.extend(p.query(t(i, 96), t(i + 1, 96), 0));
    }
    assert_eq!(whole, parts);
}

#[test]
fn query_cycles_matches_rational_query() {
    let p = pat("bd sd [hh hh]");
    assert_eq!(p.query_cycles(0.0, 1.0, 0), p.query(int(0), int(1), 0));
    assert_eq!(p.query_cycles(0.5, 2.5, 0), p.query(t(1, 2), t(5, 2), 0));
    assert_eq!(p.query_cycles(0.0, 1.0 / 3.0, 0).len(), 1);
    assert_eq!(p.query_cycles(1.0 / 3.0, 2.0 / 3.0, 0).len(), 1);
    assert!(p.query_cycles(f64::NAN, 1.0, 0).is_empty());
    assert!(p.query_cycles(1.0, 0.0, 0).is_empty());
}

#[test]
fn back_to_back_f64_queries_return_each_event_once() {
    let p = pat("bd*3, hh*7, <sd cp>(5,12)");
    let mut parts = Vec::new();
    let step = 0.013;
    let mut x = 0.0;
    while x < 4.0 {
        let next = x + step;
        parts.extend(p.query_cycles(x, next, 0));
        x = next;
    }
    assert!(parts.len() > 40);
    assert_eq!(parts, p.query_cycles(0.0, x, 0));
}

// --- Events and spans ---

#[test]
fn source_spans_point_at_atoms() {
    let p = pat("bd sd");
    let evs = p.query(int(0), int(1), 0);
    assert_eq!(evs[0].source, Span { start: 0, end: 2 });
    assert_eq!(evs[1].source, Span { start: 3, end: 5 });
    let text = |e: &Event| &p.source()[e.source.start..e.source.end];
    assert_eq!(text(&evs[1]), "sd");

    let p = pat("[bd:3 <c4 f#3>]*2");
    let evs = p.query(int(0), int(1), 0);
    let texts: Vec<&str> = evs
        .iter()
        .map(|e| &p.source()[e.source.start..e.source.end])
        .collect();
    assert_eq!(texts, ["bd:3", "c4", "bd:3", "f#3"]);
}

#[test]
fn copies_share_the_source_span() {
    for src in ["a!3", "a(3,8)", "a*3", "a _ ! !"] {
        let evs = pat(src).query(int(0), int(1), 0);
        assert_eq!(evs.len(), 3, "{src}");
        assert!(evs.iter().all(|e| e.source == Span { start: 0, end: 1 }));
    }
}

#[test]
fn ties_sort_by_source_position() {
    let evs = pat("hh*2, bd").query(int(0), int(1), 0);
    assert_eq!(evs[0].value, snd("hh"));
    assert_eq!(evs[1].value, snd("bd"));
    assert_eq!(evs[0].begin, evs[1].begin);
    let evs = pat("[e4,c4]").query(int(0), int(1), 0);
    assert_eq!(evs[0].value, note(64));
}

#[test]
fn pattern_keeps_its_source_and_is_shareable() {
    fn assert_send_sync<T: Send + Sync + Clone>(_: &T) {}
    let p = pat("bd [sd hh]");
    assert_send_sync(&p);
    assert_eq!(p.source(), "bd [sd hh]");
    let q = p.clone();
    assert_eq!(q.query(int(0), int(1), 0), p.query(int(0), int(1), 0));
}

#[test]
fn every_event_has_positive_length() {
    let p = pat("<a@3 b> [c d/2]*1.5 e(3,8,1) f _ g!2 [h,i]/3");
    for e in p.query(int(-5), int(20), 0) {
        assert!(e.begin < e.end, "{e:?}");
    }
}
