// SPDX-License-Identifier: GPL-3.0-or-later
//! Strudel code: what each supported function plays.

use sky_pattern::{Control, Event, Pattern, Time, Value, is_code, parse};

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

fn snd_i(name: &str, index: u32) -> Value {
    Value::Sound {
        name: name.to_string(),
        index,
    }
}

fn num(n: i64) -> Value {
    Value::Number(int(n))
}

fn pat(src: &str) -> Pattern {
    parse(src).unwrap_or_else(|e| panic!("{src:?} should parse: {e}"))
}

fn query(src: &str, from: Time, to: Time) -> Vec<Event> {
    pat(src).query(from, to, 0)
}

fn cycle(src: &str, c: i64) -> Vec<Event> {
    query(src, int(c), int(c + 1))
}

/// `(begin, end, value)` of each event in cycle `c`.
fn timed(src: &str, c: i64) -> Vec<(Time, Time, Value)> {
    cycle(src, c)
        .into_iter()
        .map(|e| (e.begin, e.end, e.value))
        .collect()
}

fn values(src: &str, c: i64) -> Vec<Value> {
    cycle(src, c).into_iter().map(|e| e.value).collect()
}

fn begins(src: &str, c: i64) -> Vec<Time> {
    cycle(src, c).into_iter().map(|e| e.begin).collect()
}

fn control(src: &str, c: i64, control: Control) -> Vec<Option<f64>> {
    cycle(src, c)
        .into_iter()
        .map(|e| e.controls.get(control))
        .collect()
}

/// The events of `a` and `b` agree, ignoring where they came from in the
/// text, over the first `n` cycles.
fn same(a: &str, b: &str, n: i64) {
    let strip = |src: &str| -> Vec<(Time, Time, Value)> {
        query(src, int(0), int(n))
            .into_iter()
            .map(|e| (e.begin, e.end, e.value))
            .collect()
    };
    assert_eq!(strip(a), strip(b), "{a:?} and {b:?} differ");
}

fn error(src: &str) -> (String, usize, usize) {
    match parse(src) {
        Ok(_) => panic!("{src:?} should not parse"),
        Err(e) => (e.message, e.offset, e.len),
    }
}

/// The error's message contains `text` and points at `at` in the source.
fn error_at(src: &str, at: &str, text: &str) {
    let (message, offset, len) = error(src);
    let start = src.find(at).expect("`at` is in the source");
    assert!(
        message.contains(text),
        "{src:?}: {message:?} should contain {text:?}"
    );
    assert_eq!(
        (offset, len),
        (start, at.len()),
        "{src:?}: {message:?} points at {:?}",
        &src[offset..offset + len]
    );
}

// --- Telling code from mini-notation ---

#[test]
fn code_is_told_from_notation() {
    for src in [
        "s(\"bd\")",
        "note('c3')",
        "$: s(\"bd\")",
        "let a = 1",
        "sound(`bd`)",
        "x.fast(2)",
    ] {
        assert!(is_code(src), "{src:?}");
    }
    for src in [
        "bd*2 sd", "c4 e4 g4", "bd(3,8)", "hh*1.5", "<a b>", "bd $ sd",
    ] {
        assert!(!is_code(src), "{src:?}");
    }
    assert!(pat("s(\"bd\")").is_code());
    assert!(!pat("bd").is_code());
}

// --- Strings and the functions that read them ---

#[test]
fn s_plays_mini_notation() {
    same("s(\"bd*2 [~ sd] <hh oh>\")", "bd*2 [~ sd] <hh oh>", 4);
    same("sound('bd sd:3')", "bd sd:3", 1);
    same("\"bd sd\"", "bd sd", 1);
    same("mini(\"c4 e4\")", "c4 e4", 1);
}

#[test]
fn s_reads_every_name_as_a_sound() {
    // In mini-notation `e` is a note; in s() it is a sound.
    assert_eq!(values("s(\"e f\")", 0), [snd("e"), snd("f")]);
    assert_eq!(values("e f", 0), [Value::Note(52), Value::Note(53)]);
    error_at("s(\"bd 3\")", "3", "expected a sound name");
}

#[test]
fn a_lone_dash_is_a_rest() {
    same("s(\"bd - sd -\")", "bd ~ sd ~", 1);
    same("bd - sd", "bd ~ sd", 1);
}

#[test]
fn note_plays_notes() {
    assert_eq!(
        values("note(\"c3 e3 60\")", 0),
        [Value::Note(48), Value::Note(52), Value::Note(60)]
    );
    error_at("note(\"c3 bd\")", "bd", "note");
}

#[test]
fn n_plays_numbers() {
    assert_eq!(values("n(\"0 2 -1\")", 0), [num(0), num(2), num(-1)]);
    assert_eq!(values("n(\"0 2\")", 0)[1].midi_note(), Some(2));
}

#[test]
fn notes_on_a_sound_become_its_pitch() {
    let events = cycle("note(\"c3 e3\").s(\"piano\")", 0);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].value, snd("piano"));
    assert_eq!(events[0].controls.get(Control::Note), Some(48.0));
    assert_eq!(events[1].controls.get(Control::Note), Some(52.0));
    // Either order of note and s.
    let events = cycle("s(\"piano*2\").note(\"c3 e3\")", 0);
    assert_eq!(events[1].value, snd("piano"));
    assert_eq!(events[1].controls.get(Control::Note), Some(52.0));
    // A note function's text can be a string's method too.
    assert_eq!(
        control("\"c3 e3\".note().s(\"saw\")", 0, Control::Note),
        [Some(48.0), Some(52.0)]
    );
}

#[test]
fn n_picks_a_sample() {
    assert_eq!(
        values("n(\"0 2 1\").s(\"hh\")", 0),
        [snd_i("hh", 0), snd_i("hh", 2), snd_i("hh", 1)]
    );
    assert_eq!(
        values("s(\"hh*2\").n(\"<1 3>\")", 1),
        [snd_i("hh", 3), snd_i("hh", 3)]
    );
}

#[test]
fn bank_names_sounds_after_the_bank() {
    let p = pat("s(\"bd sd, hh*2\").bank(\"RolandTR909\")");
    assert_eq!(p.query(int(0), int(1), 0)[0].value, snd("RolandTR909_bd"));
    assert_eq!(
        p.sounds(),
        ["RolandTR909_bd", "RolandTR909_hh", "RolandTR909_sd"]
    );
    // A bank's kick is still a kick.
    assert_eq!(snd("RolandTR909_bd").midi_note(), Some(36));
    let p = pat("s(\"bd\").bank(\"<A B>\")");
    assert_eq!(p.sounds(), ["A_bd", "B_bd"]);
}

#[test]
fn sounds_lists_what_the_code_can_play() {
    let p = pat("note(\"c3 e3\").s(\"<piano saw>\")");
    assert_eq!(p.sounds(), ["piano", "saw"]);
    assert_eq!(pat("bd*2 <sd cp>").sounds(), ["bd", "cp", "sd"]);
    assert!(pat("note(\"c3\")").sounds().is_empty());
}

// --- Time ---

#[test]
fn fast_and_slow() {
    same("s(\"bd sd\").fast(2)", "[bd sd]*2", 2);
    same("s(\"bd sd\").slow(2)", "[bd sd]/2", 4);
    same("s(\"bd sd\").fast(\"1.5\")", "[bd sd]*1.5", 4);
    same("fast(2, s(\"bd sd\"))", "[bd sd]*2", 2);
    same("s(\"bd\").slow(0)", "~", 1);
}

#[test]
fn a_patterned_speed_follows_its_pattern() {
    let src = "s(\"bd sd\").fast(\"<1 2>\")";
    assert_eq!(values(src, 0), [snd("bd"), snd("sd")]);
    assert_eq!(values(src, 1).len(), 4);
    let src = "s(\"hh\").fast(\"1 2\")";
    // The first half at speed 1, the second at speed 2.
    assert_eq!(begins(src, 0), [int(0), t(1, 2)]);
}

#[test]
fn early_and_late() {
    assert_eq!(begins("s(\"bd sd\").late(0.25)", 0), [t(1, 4), t(3, 4)]);
    assert_eq!(
        timed("s(\"bd sd\").early(0.25)", 0),
        [(t(1, 4), t(3, 4), snd("sd")), (t(3, 4), t(5, 4), snd("bd")),]
    );
}

#[test]
fn rev_and_palindrome() {
    assert_eq!(
        values("s(\"bd sd hh\").rev()", 0),
        [snd("hh"), snd("sd"), snd("bd")]
    );
    same("s(\"[bd sd] hh\").rev()", "hh [sd bd]", 1);
    let src = "s(\"bd sd\").palindrome()";
    assert_eq!(values(src, 0), [snd("bd"), snd("sd")]);
    assert_eq!(values(src, 1), [snd("sd"), snd("bd")]);
}

#[test]
fn every_first_of_and_last_of() {
    let src = "s(\"bd sd\").every(3, rev)";
    assert_eq!(values(src, 0), [snd("sd"), snd("bd")]);
    assert_eq!(values(src, 1), [snd("bd"), snd("sd")]);
    assert_eq!(values(src, 3), [snd("sd"), snd("bd")]);
    let src = "s(\"bd sd\").lastOf(3, x => x.fast(2))";
    assert_eq!(values(src, 0).len(), 2);
    assert_eq!(values(src, 2).len(), 4);
    let src = "s(\"bd sd\").firstOf(2, fast(2))";
    assert_eq!(values(src, 0).len(), 4);
    assert_eq!(values(src, 1).len(), 2);
    // The example from the task.
    let src = "s(\"bd*2 sd\").fast(2).every(4, rev)";
    assert_eq!(
        values(src, 0),
        [
            snd("sd"),
            snd("bd"),
            snd("bd"),
            snd("sd"),
            snd("bd"),
            snd("bd")
        ]
    );
    assert_eq!(
        values(src, 1),
        [
            snd("bd"),
            snd("bd"),
            snd("sd"),
            snd("bd"),
            snd("bd"),
            snd("sd")
        ]
    );
}

#[test]
fn ply_iter_and_chunk() {
    same("s(\"bd sd\").ply(2)", "[bd bd] [sd sd]", 1);
    let src = "s(\"a b c d\").iter(4)";
    assert_eq!(values(src, 1)[0], snd("b"));
    assert_eq!(values(src, 3)[0], snd("d"));
    let src = "s(\"a b c d\").iterBack(4)";
    assert_eq!(values(src, 1)[0], snd("d"));
    let src = "s(\"a b c d\").chunk(4, x => x.gain(0))";
    assert_eq!(
        control(src, 1, Control::Gain),
        [None, Some(0.0), None, None]
    );
}

#[test]
fn off_and_superimpose() {
    let src = "s(\"bd sd\").off(0.25, x => x.s(\"hh\"))";
    assert_eq!(
        timed(src, 0),
        [
            (int(0), t(1, 2), snd("bd")),
            (t(1, 4), t(3, 4), snd("hh")),
            (t(1, 2), int(1), snd("sd")),
            (t(3, 4), t(5, 4), snd("hh")),
        ]
    );
    assert_eq!(values("s(\"bd\").superimpose(fast(2))", 0).len(), 3);
    assert_eq!(values("s(\"bd\").layer(fast(2), fast(3))", 0).len(), 5);
}

#[test]
fn within_inside_and_outside() {
    let src = "s(\"a b c d\").within(0, 0.5, x => x.gain(0))";
    assert_eq!(
        control(src, 0, Control::Gain),
        [Some(0.0), Some(0.0), None, None]
    );
    same("s(\"a b c d\").inside(2, rev)", "s(\"b a d c\")", 1);
    same("s(\"<a b>\").outside(2, rev)", "s(\"<b a>\")", 2);
}

#[test]
fn swing_moves_every_other_slice() {
    let src = "s(\"hh*8\").swingBy(1/3, 4)";
    let b = begins(src, 0);
    assert_eq!(b[0], int(0));
    assert_eq!(b[1], t(1, 8) + t(1, 3) * t(1, 8));
    assert_eq!(b[2], t(1, 4));
}

#[test]
fn brak() {
    let src = "s(\"bd sd\").brak()";
    assert_eq!(begins(src, 0), [int(0), t(1, 2)]);
    assert_eq!(begins(src, 1), [t(5, 4), t(3, 2)]);
}

#[test]
fn cat_seq_stack_and_timecat() {
    same("cat(s(\"bd\"), s(\"sd hh\"))", "<bd [sd hh]>", 4);
    same("seq(s(\"bd\"), s(\"sd hh\"))", "bd [sd hh]", 1);
    same("stack(s(\"bd\"), s(\"hh*2\"))", "bd, hh*2", 1);
    same("s(\"bd\").stack(s(\"hh*2\"))", "bd, hh*2", 1);
    same("timecat([3, s(\"bd\")], [1, s(\"sd\")])", "bd@3 sd", 1);
    same("arrange([2, s(\"bd\")], [1, s(\"sd\")])", "<bd bd sd>", 6);
    same("note(run(4))", "0 1 2 3", 1);
}

// --- Chance ---

#[test]
fn sometimes_changes_a_share_of_the_events() {
    let src = "s(\"hh*8\").sometimes(x => x.speed(2))";
    let (mut changed, mut total) = (0, 0);
    for c in 0..50 {
        let events = cycle(src, c);
        // The changed events replace the others: never both, never neither.
        assert_eq!(events.len(), 8, "cycle {c}");
        total += events.len();
        changed += events
            .iter()
            .filter(|e| e.controls.get(Control::Speed).is_some())
            .count();
    }
    assert!((150..250).contains(&changed), "{changed} of {total}");
    assert!(
        cycle("s(\"hh*8\").always(x => x.speed(2))", 0)
            .iter()
            .all(|e| e.controls.get(Control::Speed) == Some(2.0))
    );
    assert!(
        cycle("s(\"hh*8\").never(x => x.speed(2))", 0)
            .iter()
            .all(|e| e.controls.is_empty())
    );
    let src = "s(\"hh*8\").rarely(x => x.speed(2))";
    let changed: usize = (0..50)
        .map(|c| {
            cycle(src, c)
                .iter()
                .filter(|e| e.controls.get(Control::Speed).is_some())
                .count()
        })
        .sum();
    assert!((50..150).contains(&changed), "{changed}");
}

#[test]
fn some_cycles_changes_whole_cycles() {
    let src = "s(\"hh*4\").someCycles(x => x.speed(2))";
    let mut changed = 0;
    for c in 0..40 {
        let speeds = control(src, c, Control::Speed);
        assert!(speeds.windows(2).all(|w| w[0] == w[1]), "cycle {c}");
        changed += usize::from(speeds[0].is_some());
    }
    assert!((8..32).contains(&changed), "{changed}");
}

#[test]
fn degrade_drops_events() {
    let kept: usize = (0..50)
        .map(|c| cycle("s(\"hh*8\").degradeBy(0.25)", c).len())
        .sum();
    assert!((250..350).contains(&kept), "{kept}");
    assert_eq!(cycle("s(\"hh*8\").degradeBy(0)", 0).len(), 8);
    assert_eq!(cycle("s(\"hh*8\").degradeBy(1)", 0).len(), 0);
    assert_eq!(cycle("s(\"hh*8\").undegradeBy(1)", 0).len(), 8);
    error_at("s(\"hh\").degradeBy(2)", "2", "0 to 1");
}

#[test]
fn random_choices_repeat_for_the_same_seed() {
    let p = pat("s(\"hh*16\").degrade().sometimes(x => x.gain(0))");
    let a = p.query(int(0), int(4), 7);
    assert_eq!(a, p.query(int(0), int(4), 7));
    // Split queries agree with one whole query.
    let mut split = p.query(int(0), t(3, 2), 7);
    split.extend(p.query(t(3, 2), int(4), 7));
    assert_eq!(a, split);
}

#[test]
fn randcat_picks_one_pattern_per_cycle() {
    let src = "randcat(s(\"bd\"), s(\"sd sd\"))";
    let mut lens = Vec::new();
    for c in 0..20 {
        lens.push(cycle(src, c).len());
    }
    assert!(lens.contains(&1) && lens.contains(&2), "{lens:?}");
}

// --- Structure ---

#[test]
fn struct_and_mask() {
    same("s(\"bd\").struct(\"x ~ x x\")", "bd ~ bd bd", 1);
    same("s(\"bd\").struct(\"1 0 t f\")", "bd ~ bd ~", 1);
    same("s(\"[a b c d]\").mask(\"1 0 1 1\")", "s(\"a ~ c d\")", 1);
    same(
        "s(\"a b c d\").mask(\"<1 [0 1]>\")",
        "s(\"<[a b c d] [~ ~ c d]>\")",
        2,
    );
    // struct keeps the value sounding at each step's start.
    same("note(\"c3 e3\").struct(\"x*4\")", "c3 c3 e3 e3", 1);
}

#[test]
fn euclid() {
    same("s(\"bd\").euclid(3, 8)", "bd(3,8)", 1);
    same("s(\"bd\").euclidRot(3, 8, 2)", "bd(3,8,2)", 1);
    let src = "s(\"bd\").euclidLegato(3, 8)";
    assert_eq!(
        timed(src, 0)
            .into_iter()
            .map(|(b, e, _)| (b, e))
            .collect::<Vec<_>>(),
        [(int(0), t(3, 8)), (t(3, 8), t(3, 4)), (t(3, 4), int(1))]
    );
}

#[test]
fn segment_cuts_a_signal_into_steps() {
    let src = "n(saw.segment(4).range(0, 8))";
    assert_eq!(values(src, 0), [num(0), num(2), num(4), num(6)]);
    let src = "n(saw.range(0, 8).segment(4))";
    assert_eq!(values(src, 2), [num(0), num(2), num(4), num(6)]);
    let src = "n(irand(8).segment(16))";
    for v in values(src, 3) {
        let n = v.number().unwrap();
        assert!(n == n.floor() && (0.0..8.0).contains(&n), "{n}");
    }
    error_at("s(\"bd\").segment(4)", "segment", "signal");
    error_at("sine.rev()", "sine.rev()", "segment");
}

// --- Values ---

#[test]
fn arithmetic_on_notes() {
    assert_eq!(
        values("note(\"c3 e3\").add(7)", 0),
        [Value::Note(55), Value::Note(59)]
    );
    assert_eq!(
        values("note(\"c3 e3\").add(\"<0 12>\")", 1),
        [Value::Note(60), Value::Note(64)]
    );
    assert_eq!(values("note(\"c3\").sub(12)", 0), [Value::Note(36)]);
    assert_eq!(values("n(\"1 2\").mul(3)", 0), [num(3), num(6)]);
    // A note pushed out of range is dropped.
    assert_eq!(values("note(\"c3 120\").add(12)", 0), [Value::Note(60)]);
    // On a sound, arithmetic moves its note.
    assert_eq!(
        control("note(\"c3\").s(\"piano\").add(12)", 0, Control::Note),
        [Some(60.0)]
    );
    // add with a pattern of two values in a cycle: each event takes the
    // value sounding at its start.
    assert_eq!(
        values("note(\"c3*4\").add(\"0 12\")", 0),
        [
            Value::Note(48),
            Value::Note(48),
            Value::Note(60),
            Value::Note(60)
        ]
    );
}

#[test]
fn scale_turns_degrees_into_notes() {
    assert_eq!(
        values("n(\"0 2 4 7\").scale(\"C:major\")", 0),
        [
            Value::Note(48),
            Value::Note(52),
            Value::Note(55),
            Value::Note(60)
        ]
    );
    assert_eq!(
        values("n(\"0 1\").scale(\"<C:minor A2:minor:pentatonic>\")", 1),
        [Value::Note(45), Value::Note(48)]
    );
    assert_eq!(
        values("n(\"-1\").scale(\"C4:minor\")", 0),
        [Value::Note(58)]
    );
    error_at("n(\"0\").scale(\"C:nope\")", "C:nope", "unknown scale");
    // And then a sound plays them.
    let events = cycle("n(\"0 2\").scale(\"C:major\").s(\"piano\")", 0);
    assert_eq!(events[1].value, snd("piano"));
    assert_eq!(events[1].controls.get(Control::Note), Some(52.0));
}

#[test]
fn controls_set_per_event() {
    let src = "s(\"hh*4\").gain(\".5 1\").lpf(800).resonance(10)";
    assert_eq!(
        control(src, 0, Control::Gain),
        [Some(0.5), Some(0.5), Some(1.0), Some(1.0)]
    );
    assert_eq!(control(src, 0, Control::Cutoff), [Some(800.0); 4]);
    assert_eq!(control(src, 0, Control::Resonance), [Some(10.0); 4]);
    assert_eq!(
        pat(src).controls(),
        [Control::Gain, Control::Cutoff, Control::Resonance]
    );
    let src = "note(\"c3\").s(\"saw\").attack(0.01).decay(.2).sustain(0.5).release(1).velocity(.5)";
    let e = &cycle(src, 0)[0];
    assert_eq!(e.controls.get(Control::Attack), Some(0.01));
    assert_eq!(e.controls.get(Control::Decay), Some(0.2));
    assert_eq!(e.controls.get(Control::Sustain), Some(0.5));
    assert_eq!(e.controls.get(Control::Release), Some(1.0));
    assert_eq!(e.controls.get(Control::Velocity), Some(0.5));
    assert!(cycle("bd sd", 0).iter().all(|e| e.controls.is_empty()));
}

#[test]
fn signals_set_controls_at_each_event() {
    let src = "s(\"hh*4\").lpf(sine.range(200, 2000))";
    let cutoffs: Vec<f64> = control(src, 0, Control::Cutoff)
        .into_iter()
        .map(|c| c.unwrap().round())
        .collect();
    assert_eq!(cutoffs, [1100.0, 2000.0, 1100.0, 200.0]);
    let src = "s(\"hh*4\").gain(saw.slow(2))";
    assert_eq!(
        control(src, 1, Control::Gain),
        [Some(0.5), Some(0.625), Some(0.75), Some(0.875)]
    );
    let src = "s(\"hh*2\").lpf(rand.range(100, 200))";
    for c in control(src, 0, Control::Cutoff) {
        assert!((100.0..200.0).contains(&c.unwrap()));
    }
}

#[test]
fn legato_changes_lengths() {
    assert_eq!(
        timed("note(\"c3 e3\").legato(0.5)", 0),
        [
            (int(0), t(1, 4), Value::Note(48)),
            (t(1, 2), t(3, 4), Value::Note(52)),
        ]
    );
    assert_eq!(timed("note(\"c3\").clip(2)", 0)[0].1, int(2));
}

#[test]
fn echo_repeats_quieter() {
    let src = "s(\"bd\").echo(3, 1/8, 0.5)";
    let events = cycle(src, 0);
    assert_eq!(
        events.iter().map(|e| e.begin).collect::<Vec<_>>(),
        [int(0), t(1, 8), t(1, 4)]
    );
    assert_eq!(
        events
            .iter()
            .map(|e| e.controls.get(Control::Velocity))
            .collect::<Vec<_>>(),
        [None, Some(0.5), Some(0.25)]
    );
    same(
        "s(\"bd\").stut(3, 0.5, 1/8)",
        "s(\"bd\").echo(3, 1/8, 0.5)",
        2,
    );
}

#[test]
fn hurry_speeds_up_and_raises_the_pitch() {
    let src = "s(\"bd sd\").hurry(2)";
    assert_eq!(values(src, 0).len(), 4);
    assert_eq!(control(src, 0, Control::Speed), [Some(2.0); 4]);
}

// --- Programs ---

#[test]
fn dollar_lines_play_together() {
    let src = "$: s(\"bd*4\")\n$: s(\"hh*8\").gain(.5)\n";
    assert_eq!(cycle(src, 0).len(), 12);
    let src = "$: s(\"bd*4\")\n_$: s(\"hh*8\")";
    assert_eq!(values(src, 0), vec![snd("bd"); 4]);
    let p = pat("$: s(\"bd\")\ns(\"hh\")");
    assert_eq!(p.query(int(0), int(1), 0).len(), 1);
    assert!(p.warnings()[0].message.contains("$:"));
}

#[test]
fn the_last_pattern_plays() {
    let p = pat("s(\"bd\")\ns(\"hh*2\")");
    assert_eq!(p.query(int(0), int(1), 0).len(), 2);
    assert!(
        p.warnings()[0]
            .message
            .contains("only the last pattern plays")
    );
    // Names, comments, semicolons and await.
    let src =
        "// drums\nconst kick = s(\"bd*2\"); /* hats */\nlet hats = s(\"hh*4\")\nstack(kick, hats)";
    let p = pat(src);
    assert_eq!(p.query(int(0), int(1), 0).len(), 6);
    assert!(p.warnings().is_empty());
    same(
        "await samples('github:tidalcycles/dirt-samples')\ns(\"bd\")",
        "bd",
        1,
    );
    same("setcps(0.5)\ns(\"bd\")", "bd", 1);
    assert!(
        pat("setcps(0.5)\ns(\"bd\")").warnings()[0]
            .message
            .contains("setcps")
    );
}

#[test]
fn functions_can_be_named_and_passed() {
    same(
        "const twice = x => x.fast(2)\ns(\"bd sd\").every(1, twice)",
        "[bd sd]*2",
        2,
    );
    same("s(\"bd sd\").every(1, (x) => x.fast(2))", "[bd sd]*2", 2);
    same("s(\"bd sd\").every(1, x => rev(x))", "sd bd", 1);
    same("s(\"bd sd\").fast(1 + 1)", "[bd sd]*2", 1);
    same("s(\"bd sd\").slow(4 / 2)", "[bd sd]/2", 2);
    same("s(\"bd\").late(-0.5 * -1)", "s(\"bd\").late(0.5)", 2);
    same("s(\"bd\").late(-.25 + .75)", "s(\"bd\").late(0.5)", 2);
}

#[test]
fn effects_are_left_out_with_one_warning() {
    let p = pat("s(\"bd\").room(0.5).delay(.25).room(.2).pan(sine).color('red')");
    assert_eq!(p.query(int(0), int(1), 0).len(), 1);
    assert_eq!(p.warnings().len(), 1);
    let w = &p.warnings()[0];
    assert_eq!(
        w.message,
        "room, delay, pan were ignored: effects go on the track's inserts here"
    );
    assert_eq!(&p.source()[w.span.start..w.span.end], "room");
    let p = pat("s(\"bd\").jux(rev)");
    assert_eq!(p.query(int(0), int(1), 0).len(), 2);
    assert!(p.warnings()[0].message.contains("jux"));
}

// --- Errors ---

#[test]
fn errors_point_at_the_code() {
    error_at("s(\"bd\").fsat(2)", "fsat", "unknown function");
    error_at("s(\"bd\").chop(4)", "chop", "isn't supported yet");
    error_at("s(\"bd [sd\")", "[", "");
    error_at("s(\"bd\").fast()", "s(\"bd\").fast()", "fast");
    error_at("s(\"bd\").fast(\"x\")", "x", "");
    for src in ["s(\"bd\"", "s(\"bd\").fast(2"] {
        // The error points at the last character.
        assert_eq!(error(src).1, src.len() - 1, "{src:?} ends too soon");
    }
    error_at("note(\"c3\").add(\"bd\")", "bd", "");
    error_at("s(\"bd\").every(4)", "s(\"bd\").every(4)", "every");
    error_at("x.fast(2)", "x", "");
    error_at(
        "s(\"bd\").fast(100000)",
        "s(\"bd\").fast(100000)",
        "speed factor",
    );
    error_at("s(\"bd\") == 1", "==", "");
}

#[test]
fn an_error_inside_a_string_counts_columns_in_its_line() {
    let e = parse("$: s(\"bd\")\n$: s(\"hh [\")").unwrap_err();
    assert_eq!(e.column, 10);
}

#[test]
fn runaway_code_is_refused() {
    // Each line doubles the last.
    let mut src = String::from("let a0 = s(\"bd\")\n");
    for i in 0..40 {
        src.push_str(&format!("let a{} = stack(a{i}, a{i})\n", i + 1));
    }
    src.push_str("a40");
    assert!(parse(&src).is_err());
    assert!(parse("s(\"bd*64\").fast(1000).ply(1000)").is_err());
    // Deep nesting is an error, not a stack overflow.
    let deep = format!("{}s(\"bd\"){}", "stack(".repeat(500), ")".repeat(500));
    assert!(parse(&deep).is_err());
}

#[test]
fn a_pasted_strudel_piece_plays() {
    let src = r#"samples('github:tidalcycles/dirt-samples')
$: s("bd*2 [~ sd] hh*4").bank("RolandTR909").every(4, rev)
$: note("<c3 eb3 g3>*4").s("sawtooth").lpf(sine.range(400, 2000).slow(4)).gain(".6 1")
$: n("0 2 4 7").scale("C:minor").off(1/8, x => x.add(12)).room(.4)"#;
    let p = pat(src);
    assert_eq!(p.query(int(0), int(1), 0).len(), 7 + 4 + 8);
    assert_eq!(p.warnings().len(), 1);
    assert!(p.sounds().contains(&"RolandTR909_bd".to_string()));
}
