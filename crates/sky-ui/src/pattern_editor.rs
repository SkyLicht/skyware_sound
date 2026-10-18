// SPDX-License-Identifier: GPL-3.0-or-later
//! The Editor tab: a header for the selected clip, then the piano roll, the
//! pattern editor or the audio clip settings, depending on the clip.

use std::collections::{BTreeMap, HashMap};
use std::ops::Range;

use egui::text::{CCursor, CCursorRange};
use egui::{
    Align2, Button, Color32, ComboBox, Context, CornerRadius, DragValue, EventFilter, Frame, Id,
    Key, Modifiers, Order, Rect, RichText, ScrollArea, Sense, Slider, Stroke, StrokeKind, TextEdit,
    Ui, vec2,
};
use sky_core::{ClipId, PPQ, Ticks, TrackId};
use sky_dsp::InstrumentType;
use sky_library::samples::{blank_calls, strip_calls};
use sky_library::{PackSource, Samples, find_samples};
use sky_model::command::VELOCITY;
use sky_model::{Clip, ClipContent, Command, Origin, Project, SampleBank, Swing, Track};
use sky_pattern::{Control, Span, Time};
use sky_session::{SoundsLoaded, SoundsRequest, pattern_sound_names};

use crate::actions::{self, AudioClipChange};
use crate::app::App;
use crate::jobs::JobOutput;
use crate::live_pattern::{self, Grid, RESOLUTIONS, Row};
use crate::theme;

#[derive(Default)]
pub struct PatternState {
    pub clip: Option<ClipId>,
    pub text: String,
    /// The source, seed and cycle the text box was loaded from.
    synced: Option<(String, u64, Ticks)>,
    /// The clip name being edited.
    pub name: Option<(ClipId, String)>,
    /// The text of each control pattern being edited.
    pub controls: HashMap<String, String>,
    /// The clip and controls the control texts were loaded from.
    controls_synced: Option<(ClipId, BTreeMap<String, String>)>,
    /// The names offered for the word before the caret, as last drawn.
    completion: Option<Completion>,
    /// The offered name the arrow keys are on.
    pick: usize,
    /// Escape hid the names for this text with the caret here.
    dismissed: Option<(String, usize)>,
    /// Where the names were drawn last frame.
    popup: Option<Rect>,
    /// The grid's step count, once picked.
    pub grid_steps: Option<usize>,
    /// Grid rows added or emptied on the grid, by clip: the text can't
    /// hold a row with no hits.
    pub grid_rows: Vec<(ClipId, String)>,
    /// The clip whose pattern waits for the sounds it asked for.
    pub loading: Option<ClipId>,
}

#[cfg(test)]
impl PatternState {
    /// The names offered for the word before the caret, and the one the
    /// arrow keys are on.
    pub fn offered(&self) -> (&[String], usize) {
        match &self.completion {
            Some(c) => (&c.names, self.pick),
            None => (&[], 0),
        }
    }
}

/// Names that complete the word before the caret.
#[derive(Clone, Debug, PartialEq)]
struct Completion {
    /// Where the word starts, in characters.
    start: usize,
    caret: usize,
    names: Vec<String>,
}

/// Moves the text box's caret to character `caret`.
fn set_caret(ctx: &Context, id: Id, caret: usize) {
    let mut state = TextEdit::load_state(ctx, id).unwrap_or_default();
    state
        .cursor
        .set_char_range(Some(CCursorRange::one(CCursor::new(caret))));
    TextEdit::store_state(ctx, id, state);
}

pub fn clip_editor(app: &mut App, ui: &mut Ui) {
    let view = app.view.clone();
    let project = &view.project;
    let Some((track, clip)) = app.sel.clip.and_then(|c| project.find_clip(c).ok()) else {
        ui.add_space(8.0);
        ui.label(RichText::new("Select a clip to edit it here.").color(theme::TEXT_WEAK));
        ui.label(
            RichText::new(
                "Double-click an empty spot on an instrument track to create a one-bar MIDI clip, \
                 or right-click a lane for more.",
            )
            .small()
            .color(theme::TEXT_DIM),
        );
        return;
    };
    clip_header(app, ui, project, track, clip);
    match &clip.content {
        ClipContent::Midi { notes } => {
            crate::piano_roll::show(app, ui, project, track, clip, notes)
        }
        ClipContent::Pattern {
            source,
            seed,
            cycle,
            controls,
            swing,
        } => {
            ScrollArea::vertical()
                .id_salt(("pattern_scroll", clip.id))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    pattern_ui(app, ui, project, track, clip, source, *seed, *cycle);
                    groove_ui(app, ui, track, clip, controls, *swing);
                });
        }
        ClipContent::Steps(steps) => crate::step_editor::show(app, ui, track, clip, steps),
        ClipContent::Audio {
            source,
            gain_db,
            fade_in,
            fade_out,
            ..
        } => audio_ui(
            app, ui, project, clip, *source, *gain_db, *fade_in, *fade_out,
        ),
    }
}

fn clip_header(app: &mut App, ui: &mut Ui, project: &Project, track: &Track, clip: &Clip) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(10.0, 18.0), Sense::hover());
        ui.painter()
            .rect_filled(rect, CornerRadius::same(2), theme::track_color(track.color));
        ui.label(RichText::new(&track.name).color(theme::TEXT_WEAK));
        ui.label(RichText::new("›").color(theme::TEXT_DIM));

        // Clip name.
        let editing = app
            .pattern
            .name
            .as_ref()
            .is_some_and(|(id, _)| *id == clip.id);
        if !editing {
            app.pattern.name = Some((clip.id, clip.name.clone()));
        }
        let mut commit = None;
        if let Some((_, text)) = app.pattern.name.as_mut() {
            let resp = ui.add(
                TextEdit::singleline(text)
                    .desired_width(160.0)
                    .id(Id::new(("clip_name", clip.id))),
            );
            if resp.lost_focus() && text.trim() != clip.name {
                commit = Some(text.clone());
            }
            if !resp.has_focus() && !resp.lost_focus() && *text != clip.name {
                *text = clip.name.clone();
            }
        }
        if let Some(name) = commit {
            let r = actions::rename_clip(&app.session, clip.id, &name);
            app.report(r);
        }

        ui.separator();
        let tpb = project.ticks_per_bar().max(1) as f64;
        ui.label(RichText::new("Length").color(theme::TEXT_WEAK));
        let mut bars = clip.length as f64 / tpb;
        let resp = ui
            .add(
                DragValue::new(&mut bars)
                    .range(1.0 / 16.0..=1024.0)
                    .speed(0.02)
                    .max_decimals(3)
                    .suffix(" bars"),
            )
            .on_hover_text("Clip length in bars");
        if resp.changed() {
            let len = ((bars * tpb).round() as Ticks).max(1);
            if len != clip.length {
                let key = app.gestures.key(resp.id);
                let r = actions::apply(
                    &app.session,
                    sky_model::Transaction::single(sky_model::Command::SetClipLength {
                        id: clip.id,
                        length: len,
                    }),
                    Some(key),
                );
                app.report(r);
            }
        }
        ui.label(
            RichText::new(format!(
                "starts at {}",
                project.time_signature.format_position(clip.start)
            ))
            .small()
            .color(theme::TEXT_DIM),
        );
    });
    ui.add_space(2.0);
}

/// The pattern's parse result, for the status line.
#[derive(Debug)]
pub enum ParseStatus {
    Ok {
        events: usize,
        /// What Strudel code asked for that the pattern leaves out.
        warnings: Vec<String>,
    },
    Error {
        message: String,
        /// 1-based, counted in lines of the text.
        line: usize,
        column: usize,
    },
}

impl ParseStatus {
    /// Where the error is, as "Line 2, column 5", or "Column 5" in one-line
    /// text.
    fn place(&self, lines: usize) -> String {
        match self {
            ParseStatus::Error { line, column, .. } if lines > 1 => {
                format!("Line {line}, column {column}")
            }
            ParseStatus::Error { column, .. } => format!("Column {column}"),
            ParseStatus::Ok { .. } => String::new(),
        }
    }
}

/// The pattern's parse result. Its `samples(...)` calls are not
/// mini-notation, so they are read on their own.
pub fn parse_status(source: &str, seed: u64) -> ParseStatus {
    let line = |offset: usize| source[..offset.min(source.len())].matches('\n').count() + 1;
    let plain = match find_samples(source) {
        Ok(calls) => blank_calls(source, &calls),
        Err(e) => {
            let at = e.range.start.min(source.len());
            let line_start = source[..at].rfind('\n').map_or(0, |i| i + 1);
            return ParseStatus::Error {
                line: line(at),
                message: e.message,
                column: source[line_start..at].chars().count() + 1,
            };
        }
    };
    match sky_pattern::parse(&plain) {
        Ok(p) => ParseStatus::Ok {
            events: p
                .query(Time::from_integer(0), Time::from_integer(1), seed)
                .len(),
            warnings: p.warnings().iter().map(|w| w.message.clone()).collect(),
        },
        Err(e) => ParseStatus::Error {
            line: line(e.offset),
            message: e.message,
            column: e.column,
        },
    }
}

/// The parts of pattern text that aren't mini-notation, sorted: its
/// `samples(...)` calls and, in Strudel code, everything outside the
/// strings. With them, where the text fails to parse.
fn code_spans(text: &str) -> (Vec<Range<usize>>, Option<Range<usize>>) {
    let calls = match find_samples(text) {
        Ok(calls) => calls,
        Err(e) => return (Vec::new(), Some(e.range)),
    };
    let plain = blank_calls(text, &calls);
    let error = sky_pattern::parse(&plain)
        .err()
        .map(|e| e.offset..e.offset + e.len);
    let mut spans: Vec<Range<usize>> = calls.into_iter().map(|c| c.range).collect();
    spans.extend(sky_pattern::code_ranges(&plain));
    spans.sort_by_key(|r| r.start);
    let mut merged: Vec<Range<usize>> = Vec::new();
    for r in spans {
        match merged.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => merged.push(r),
        }
    }
    (merged, error)
}

/// The track's sample bank, when it is one.
fn bank_of(track: &Track) -> Option<&SampleBank> {
    track
        .instrument
        .as_ref()
        .filter(|s| s.kind == InstrumentType::Bank)
        .and_then(|s| s.bank.as_ref())
}

/// What applying `text` must load first, with the pattern as it is without
/// its `samples(...)` calls; `None` when it can change at once. A sound the
/// bank lacks is worth loading while a linked pack may have it: until the
/// pack is read, or once it is read and has it.
fn sounds_to_load(app: &App, track: &Track, text: &str) -> Option<(SoundsRequest, String)> {
    let calls = find_samples(text).ok()?;
    let source = strip_calls(text, &calls);
    let mut request = SoundsRequest {
        names: pattern_sound_names(&source),
        ..Default::default()
    };
    for call in calls {
        match call.samples {
            Samples::Address(a) => request.link.push(PackSource::parse(&a, None)),
            Samples::Map(m) => request.maps.push(m),
        }
    }
    if request.link.is_empty() && request.maps.is_empty() {
        let bank = bank_of(track).filter(|b| !b.packs.is_empty())?;
        let worth = request
            .names
            .iter()
            .filter(|n| bank.resolve(n).is_none())
            .any(|n| {
                bank.packs
                    .iter()
                    .any(|p| match app.browser.listing(&p.map) {
                        Some(l) => l.pack.map.find_sound(n).is_some(),
                        None => !app.browser.pack_failed(&p.map),
                    })
            });
        if !worth {
            return None;
        }
    }
    Some((request, source))
}

/// One line on what loading a pattern's sounds did.
fn sounds_summary(done: &SoundsLoaded) -> String {
    let names = |v: &[String]| {
        let mut s = v.iter().take(8).cloned().collect::<Vec<_>>().join(", ");
        if v.len() > 8 {
            s.push_str(&format!(" and {} more", v.len() - 8));
        }
        s
    };
    let mut parts = Vec::new();
    for name in &done.linked {
        let unknown = done
            .packs
            .iter()
            .any(|p| &p.name == name && p.license == sky_model::License::Unknown);
        parts.push(if unknown {
            format!("Linked {name}, whose license is unknown, so exports will warn")
        } else {
            format!("Linked {name}")
        });
    }
    let loaded: Vec<String> = done.loaded.iter().map(|(n, _)| n.clone()).collect();
    if !loaded.is_empty() {
        parts.push(format!("loaded {}", names(&loaded)));
    }
    if !done.missing.is_empty() {
        parts.push(format!("no linked pack has {}", names(&done.missing)));
    }
    if let Some(first) = done.failed.first() {
        parts.push(format!(
            "{} could not load, such as {first}",
            done.failed.len()
        ));
    }
    if done.offline {
        parts.push("the network is down, so saved copies of the packs were used".into());
    }
    let mut text = parts.join("; ");
    if let Some(c) = text.get(..1) {
        text = c.to_uppercase() + &text[1..];
    }
    if text.is_empty() {
        "The pattern's sounds are all in the bank".into()
    } else {
        text
    }
}

impl App {
    /// Loads what a pattern's text asks for in the background, then changes
    /// the pattern to `source`, as one undo step.
    #[allow(clippy::too_many_arguments)]
    fn start_pattern_sounds(
        &mut self,
        ctx: &Context,
        track: TrackId,
        clip: ClipId,
        text: String,
        source: String,
        request: SoundsRequest,
        seed: u64,
        cycle: Ticks,
    ) {
        let project = self.session.project();
        let Ok((_, c)) = project.find_clip(clip) else {
            return;
        };
        let ClipContent::Pattern {
            controls, swing, ..
        } = &c.content
        else {
            return;
        };
        let then = vec![Command::SetPattern {
            clip,
            source: source.clone(),
            seed,
            cycle: cycle.max(1),
            controls: controls.clone(),
            swing: *swing,
        }];
        let label = format!("Load sounds for {}", c.name);
        self.pattern.loading = Some(clip);
        let session = self.session.clone();
        self.jobs.spawn(ctx, Some(format!("{label}…")), move || {
            let result = session
                .load_pattern_sounds(track, &request, &label, then, Origin::User)
                .map_err(|e| e.to_string());
            JobOutput::PatternSounds {
                clip,
                text,
                source,
                seed,
                cycle,
                result,
            }
        });
    }

    pub fn pattern_sounds_loaded(
        &mut self,
        clip: ClipId,
        text: &str,
        source: String,
        seed: u64,
        cycle: Ticks,
        result: Result<SoundsLoaded, String>,
    ) {
        if self.pattern.loading == Some(clip) {
            self.pattern.loading = None;
        }
        self.refresh();
        match result {
            Ok(done) => {
                // The box still holds what was applied: show the pattern as
                // it is now, without its samples(...) lines.
                if self.pattern.clip == Some(clip) && self.pattern.text == text {
                    self.pattern.text = source.clone();
                    self.pattern.synced = Some((source, seed, cycle));
                }
                let summary = sounds_summary(&done);
                if done.failed.is_empty() {
                    self.info(summary);
                } else {
                    self.error(summary);
                }
            }
            Err(e) => self.error(format!("Pattern not applied: {e}")),
        }
    }
}

/// Where in the clip's pattern the playhead is, in cycles, while playing.
fn playing_at(app: &App, clip: &Clip, cycle: Ticks) -> Option<f64> {
    if !app.transport.playing {
        return None;
    }
    live_pattern::cycle_at(clip, cycle, app.transport.position_ticks)
}

/// The atoms of a pattern clip sounding now, as byte spans of its source:
/// none unless playing, or while the box holds text that isn't what plays.
#[cfg(test)]
pub fn lit_atoms(app: &App, clip: ClipId) -> Vec<Span> {
    let project = app.session.project();
    let Ok((_, clip)) = project.find_clip(clip) else {
        return Vec::new();
    };
    match &clip.content {
        ClipContent::Pattern {
            source,
            seed,
            cycle,
            ..
        } => sounding_atoms(app, clip, source, *seed, *cycle),
        _ => Vec::new(),
    }
}

fn sounding_atoms(app: &App, clip: &Clip, source: &str, seed: u64, cycle: Ticks) -> Vec<Span> {
    let at = playing_at(app, clip, cycle);
    match (at, sky_pattern::parse(source)) {
        (Some(at), Ok(p)) if app.pattern.text == source => live_pattern::sounding(&p, at, seed)
            .into_iter()
            .map(|e| e.source)
            .collect(),
        _ => Vec::new(),
    }
}

#[allow(clippy::too_many_arguments)]
fn pattern_ui(
    app: &mut App,
    ui: &mut Ui,
    project: &Project,
    track: &Track,
    clip: &Clip,
    source: &str,
    seed: u64,
    cycle: Ticks,
) {
    let ctx = ui.ctx().clone();
    let text_id = Id::new(("pattern_text", clip.id));
    let focused = ctx.memory(|m| m.has_focus(text_id));
    let current = (source.to_string(), seed, cycle);
    let st = &mut app.pattern;
    if st.clip != Some(clip.id) {
        st.completion = None;
        st.dismissed = None;
        st.grid_steps = None;
    }
    if st.clip != Some(clip.id) || (!focused && st.synced.as_ref() != Some(&current)) {
        st.clip = Some(clip.id);
        st.text = source.to_string();
        st.synced = Some(current);
    }
    if !focused {
        st.completion = None;
    }

    // Keys the box would otherwise take: Ctrl+Enter applies, and while
    // names are offered the arrows pick one, Tab takes it and Escape hides
    // them.
    let mut apply = false;
    if focused {
        apply = ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Enter));
        if let Some(c) = st.completion.clone() {
            let (up, down, tab, escape) = ui.input_mut(|i| {
                (
                    i.consume_key(Modifiers::NONE, Key::ArrowUp),
                    i.consume_key(Modifiers::NONE, Key::ArrowDown),
                    i.consume_key(Modifiers::NONE, Key::Tab),
                    i.consume_key(Modifiers::NONE, Key::Escape),
                )
            });
            let n = c.names.len().max(1);
            if down {
                st.pick = (st.pick + 1) % n;
            }
            if up {
                st.pick = (st.pick + n - 1) % n;
            }
            if escape {
                st.dismissed = Some((st.text.clone(), c.caret));
                st.completion = None;
            } else if tab && let Some(name) = c.names.get(st.pick.min(n - 1)) {
                let caret = live_pattern::complete(&mut st.text, c.start, c.caret, name);
                set_caret(&ctx, text_id, caret);
                st.completion = None;
            }
        }
    }
    let offering = st.completion.is_some();

    // The atoms sounding now, while the text is what plays.
    let at = playing_at(app, clip, cycle);
    let lit = sounding_atoms(app, clip, source, seed, cycle);
    let font = theme::mono(15.0);
    let mut layouter = |ui: &Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
        let text = buf.as_str();
        let (code, error) = code_spans(text);
        let lit: &[Span] = if text == source { &lit } else { &[] };
        let mut job = live_pattern::layout(text, &font, lit, error, &code);
        job.wrap.max_width = wrap;
        ui.ctx().fonts_mut(|f| f.layout_job(job))
    };

    let links = bank_of(track).map(|b| b.packs.clone()).unwrap_or_default();
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Pattern").color(theme::TEXT_WEAK));
        if !links.is_empty() {
            ui.label(
                RichText::new("· sounds you name load from")
                    .small()
                    .color(theme::TEXT_DIM),
            );
            for link in &links {
                ui.label(RichText::new(&link.name).small().color(theme::TEXT_WEAK))
                    .on_hover_text(format!("{}\n{}", link.map, link.license.label()));
            }
        }
    });
    // Read the linked packs, to offer their sounds' names.
    for link in &links {
        if !app.browser.pack_asked(&link.map) {
            let source = PackSource {
                name: link.name.clone(),
                map: link.map.clone(),
                license: link.license,
                credit: link.credit.clone(),
            };
            app.read_pack_listing(&ctx, source);
        }
    }
    let out = TextEdit::multiline(&mut app.pattern.text)
        .id(text_id)
        .font(theme::mono(15.0))
        .desired_width(f32::INFINITY)
        .desired_rows(2)
        .event_filter(EventFilter {
            tab: offering,
            escape: offering,
            horizontal_arrows: true,
            vertical_arrows: true,
        })
        .hint_text("bd*4, ~ sd ~ sd, hh*8")
        .layouter(&mut layouter)
        .show(ui);
    let has_focus = out.response.response.has_focus();
    let lost = out.response.response.lost_focus();

    // Offer the track's sound names for the word before the caret, then
    // those of its linked packs.
    let mut names = live_pattern::sound_names(track);
    for link in &links {
        if let Some(l) = app.browser.listing(&link.map) {
            for name in l.pack.map.sounds.keys() {
                if !names.iter().any(|n| n.eq_ignore_ascii_case(name)) {
                    names.push(name.clone());
                }
            }
        }
    }
    let st = &mut app.pattern;
    let caret = out
        .cursor_range
        .filter(|r| r.is_empty() && has_focus)
        .map(|r| r.primary.index.0);
    let next = caret.and_then(|caret| {
        let (start, word) = live_pattern::word_before(&st.text, caret)?;
        if st.dismissed.as_ref() == Some(&(st.text.clone(), caret)) {
            return None;
        }
        // In Strudel code, sounds are named inside its strings.
        let byte = st
            .text
            .char_indices()
            .nth(start)
            .map_or(st.text.len(), |(b, _)| b);
        if code_spans(&st.text).0.iter().any(|r| r.contains(&byte)) {
            return None;
        }
        let names = live_pattern::completions(&word, &names);
        (!names.is_empty()).then_some(Completion {
            start,
            caret,
            names,
        })
    });
    if next.as_ref().map(|c| &c.names) != st.completion.as_ref().map(|c| &c.names) {
        st.pick = 0;
    }
    st.completion = next;
    // A press on the names takes the focus from the box; that is not the
    // end of the edit.
    let over_names = st
        .popup
        .zip(ui.input(|i| i.pointer.interact_pos()))
        .is_some_and(|(r, p)| r.contains(p));
    // The box gives up the focus when the press ends, after the name is
    // taken, so the names' place is kept until then.
    if !ui.input(|i| i.pointer.any_down() || i.pointer.any_released()) {
        st.popup = None;
    }
    if lost && over_names {
        ctx.memory_mut(|m| m.request_focus(text_id));
    }
    if let Some(c) = st.completion.clone() {
        let caret_rect = out.galley.pos_from_cursor(CCursor::new(c.caret));
        let at = out.galley_pos + caret_rect.left_bottom().to_vec2() + vec2(0.0, 2.0);
        let mut taken = None;
        let area = egui::Area::new(Id::new(("pattern_names", clip.id)))
            .order(Order::Foreground)
            .fixed_pos(at)
            .show(&ctx, |ui| {
                Frame::popup(ui.style()).show(ui, |ui| {
                    for (i, name) in c.names.iter().enumerate() {
                        let r = ui.add(
                            Button::selectable(
                                i == st.pick,
                                RichText::new(name).font(theme::mono(13.0)),
                            )
                            .min_size(vec2(120.0, 0.0)),
                        );
                        if r.is_pointer_button_down_on() {
                            taken = Some(name.clone());
                        }
                    }
                    ui.label(
                        RichText::new("Tab completes · Esc hides")
                            .small()
                            .color(theme::TEXT_DIM),
                    );
                });
            });
        st.popup = Some(area.response.rect);
        if let Some(name) = taken {
            let caret = live_pattern::complete(&mut st.text, c.start, c.caret, &name);
            set_caret(&ctx, text_id, caret);
            ctx.memory_mut(|m| m.request_focus(text_id));
            st.completion = None;
        }
    }

    let status = parse_status(&app.pattern.text, seed);
    let lines = app.pattern.text.lines().count();
    let changed = app.pattern.text != source;
    let loading = app.pattern.loading == Some(clip.id);
    ui.horizontal(|ui| match &status {
        _ if loading => {
            ui.spinner();
            ui.label(RichText::new("Loading the sounds it names…").color(theme::TEXT_WEAK));
        }
        ParseStatus::Ok { events, warnings } => {
            ui.label(
                RichText::new(format!(
                    "Valid · {events} event{} per cycle",
                    if *events == 1 { "" } else { "s" }
                ))
                .color(theme::OK),
            );
            for w in warnings {
                ui.label(RichText::new(w).small().color(theme::WARN));
            }
            if changed {
                ui.label(
                    RichText::new("Ctrl+Enter or click away to apply")
                        .small()
                        .color(theme::TEXT_DIM),
                );
            }
        }
        ParseStatus::Error { message, .. } => {
            ui.label(
                RichText::new(format!("{}: {message}", status.place(lines))).color(theme::ERROR),
            );
        }
    });
    if (apply || (lost && !over_names)) && changed && !loading {
        match &status {
            ParseStatus::Ok { .. } => {
                let text = app.pattern.text.clone();
                if let Some((request, new_source)) = sounds_to_load(app, track, &text) {
                    app.start_pattern_sounds(
                        &ctx, track.id, clip.id, text, new_source, request, seed, cycle,
                    );
                } else {
                    let r = actions::set_pattern(&app.session, clip.id, &text, seed, cycle, None);
                    if app.report(r).is_some() {
                        app.pattern.synced = Some((text, seed, cycle));
                    }
                }
            }
            ParseStatus::Error { message, .. } => {
                app.error(format!(
                    "Pattern not applied: {message} at {}",
                    status.place(lines).to_lowercase()
                ));
            }
        }
    }

    ui.horizontal(|ui| {
        let tpb = project.ticks_per_bar().max(1) as f64;
        ui.label(RichText::new("Cycle").color(theme::TEXT_WEAK));
        let mut bars = cycle as f64 / tpb;
        let r1 = ui
            .add(
                DragValue::new(&mut bars)
                    .range(1.0 / 16.0..=64.0)
                    .speed(0.01)
                    .max_decimals(3)
                    .suffix(" bars"),
            )
            .on_hover_text("How long one repetition of the pattern lasts");
        ui.label(RichText::new("Seed").color(theme::TEXT_WEAK));
        let mut new_seed = seed;
        let r2 = ui
            .add(
                DragValue::new(&mut new_seed)
                    .range(0..=u32::MAX as u64)
                    .speed(0.2),
            )
            .on_hover_text("Changes the random choices of ? and |");
        let new_cycle = ((bars * tpb).round() as Ticks).max(1);
        if (r1.changed() && new_cycle != cycle) || (r2.changed() && new_seed != seed) {
            let id = if r1.changed() { r1.id } else { r2.id };
            let key = app.gestures.key(id);
            let r = actions::set_pattern(
                &app.session,
                clip.id,
                source,
                new_seed,
                new_cycle,
                Some(key),
            );
            app.report(r);
        }
        if app.pattern.text != source && ui.button("Revert").clicked() {
            app.pattern.text = source.to_string();
        }
        ui.separator();
        ui.checkbox(&mut app.workspace.pattern_grid, "Step grid")
            .on_hover_text("Click steps on and off instead of typing");
    });
    if app.workspace.pattern_grid {
        grid_ui(app, ui, track, clip, source, seed, cycle, at);
    }
    ui.add_space(4.0);
    pattern_preview(
        ui,
        &app.pattern.text,
        seed,
        (clip.length as f64 / cycle.max(1) as f64).ceil() as i64,
        at.filter(|_| app.pattern.text == source),
    );
    ui.collapsing("Syntax", |ui| {
        for (code, what) in [
            ("bd sd hh", "steps share the cycle equally"),
            ("~", "a rest"),
            ("[sd sd]", "a group in one step"),
            ("bd*2, hh*4", "layers played together; a new line works too"),
            ("<c e g>", "one per cycle, in turn"),
            ("hh*8?", "drop events at random"),
            ("bd(3,8)", "Euclidean rhythm"),
            ("c4 e4 g4", "notes; 60 64 67 also works"),
            (
                "samples('github:user/repo')",
                "on a line of its own, as in Strudel: the sounds you name load from that pack",
            ),
            (
                "s(\"bd*2 sd\").fast(2)",
                "Strudel code: mini-notation in quotes, functions after it",
            ),
            (
                ".every(4, rev)",
                "change every 4th cycle; x => x.add(12) works too",
            ),
            (
                "note(\"c3 e3\").s(\"sawtooth\")",
                "notes on a sound; a synth's wave follows sawtooth, square, triangle, sine",
            ),
            (
                ".gain(\".5 1\").lpf(800)",
                "gain, filter and envelope per note",
            ),
            ("$: s(\"hh*8\")", "lines starting with $: play together"),
        ] {
            ui.horizontal(|ui| {
                ui.label(RichText::new(code).monospace().color(theme::TEXT));
                ui.label(RichText::new(what).small().color(theme::TEXT_WEAK));
            });
        }
    });
}

const CELL_H: f32 = 18.0;
const CELL_GAP: f32 = 2.0;
const ROW_LABEL_W: f32 = 64.0;

/// The step grid: one row per sound or note, a click turns a step on or
/// off, and each click writes the pattern again as one undo step.
#[allow(clippy::too_many_arguments)]
fn grid_ui(
    app: &mut App,
    ui: &mut Ui,
    track: &Track,
    clip: &Clip,
    source: &str,
    seed: u64,
    cycle: Ticks,
    at: Option<f64>,
) {
    let note = |ui: &mut Ui, text: &str| {
        ui.label(RichText::new(text).small().color(theme::TEXT_DIM));
    };
    if app.pattern.text != source {
        note(ui, "Apply or revert what you typed to use the grid");
        return;
    }
    let mut grid = match live_pattern::grid_of(source, seed, app.pattern.grid_steps) {
        Ok(g) => g,
        Err(why) => {
            note(ui, why);
            return;
        }
    };
    for (_, name) in app
        .pattern
        .grid_rows
        .iter()
        .filter(|(id, _)| *id == clip.id)
    {
        if !grid.rows.iter().any(|r| r.name == *name) {
            grid.rows.push(Row {
                name: name.clone(),
                hits: vec![None; grid.steps],
            });
        }
    }
    let steps = grid.steps;
    let mut changed: Option<Grid> = None;
    ui.horizontal(|ui| {
        ui.label(RichText::new("Steps").color(theme::TEXT_WEAK));
        ComboBox::from_id_salt(("grid_steps", clip.id))
            .selected_text(steps.to_string())
            .width(48.0)
            .show_ui(ui, |ui| {
                for s in RESOLUTIONS {
                    if live_pattern::grid_fits(source, seed, s)
                        && ui.selectable_label(s == steps, s.to_string()).clicked()
                    {
                        app.pattern.grid_steps = Some(s);
                    }
                }
            })
            .response
            .on_hover_text("Steps per cycle; only the counts that keep every hit are offered");
        let names: Vec<String> = live_pattern::row_names(track)
            .into_iter()
            .filter(|n| !grid.rows.iter().any(|r| r.name.eq_ignore_ascii_case(n)))
            .collect();
        let mut added = None;
        ComboBox::from_id_salt(("grid_add", clip.id))
            .selected_text("Add row…")
            .width(100.0)
            .height(300.0)
            .show_ui(ui, |ui| {
                for n in &names {
                    if ui.selectable_label(false, n).clicked() {
                        added = Some(n.clone());
                    }
                }
            });
        if let Some(n) = added {
            app.pattern.grid_rows.push((clip.id, n));
        }
    });

    let playing = at.map(|a| ((a.fract() * steps as f64).floor() as usize).min(steps - 1));
    let room = ui.available_width() - ROW_LABEL_W - 28.0;
    let cell_w = (room / steps as f32 - CELL_GAP).clamp(8.0, 30.0);
    let color = theme::track_color(track.color);
    for (ri, row) in grid.rows.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = CELL_GAP;
            ui.add_sized(
                vec2(ROW_LABEL_W, CELL_H),
                egui::Label::new(RichText::new(&row.name).font(theme::mono(13.0))).truncate(),
            );
            for step in 0..steps {
                let (rect, _) = ui.allocate_exact_size(vec2(cell_w, CELL_H), Sense::hover());
                let id = Id::new(("grid_cell", clip.id, ri, step));
                let resp = ui.interact(rect, id, Sense::click());
                let (on, starts) = row.at(step);
                let group = step * 4 / steps;
                let lane = if group % 2 == 0 {
                    theme::LANE
                } else {
                    theme::LANE_ALT
                };
                let painter = ui.painter();
                painter.rect_filled(
                    rect,
                    CornerRadius::same(2),
                    if starts { color } else { lane },
                );
                // A held step is a bar from the hit before it.
                if on && !starts {
                    let bar = Rect::from_min_max(
                        rect.left_center() - vec2(CELL_GAP, 2.0),
                        rect.right_center() + vec2(0.0, 2.0),
                    );
                    painter.rect_filled(bar, CornerRadius::ZERO, color.gamma_multiply(0.7));
                }
                if playing == Some(step) {
                    painter.rect_stroke(
                        rect,
                        CornerRadius::same(2),
                        Stroke::new(1.5, theme::ACCENT),
                        StrokeKind::Inside,
                    );
                } else if resp.hovered() {
                    painter.rect_stroke(
                        rect,
                        CornerRadius::same(2),
                        Stroke::new(1.0, theme::TEXT_WEAK),
                        StrokeKind::Inside,
                    );
                }
                if resp.clicked() {
                    let mut g = grid.clone();
                    g.rows[ri].toggle(step);
                    changed = Some(g);
                }
            }
            if ui
                .small_button("×")
                .on_hover_text("Remove this row")
                .clicked()
            {
                let mut g = grid.clone();
                g.rows.remove(ri);
                changed = Some(g);
            }
        });
    }
    if grid.rows.is_empty() {
        note(ui, "Add a row to start a beat");
    }
    if let Some(g) = changed {
        let text = live_pattern::grid_text(&g);
        app.pattern.grid_steps = Some(g.steps);
        // Removing an empty row leaves the text as it is.
        if text != live_pattern::grid_text(&grid) {
            let r = actions::set_pattern(&app.session, clip.id, &text, seed, cycle, None);
            if app.report(r).is_some() {
                app.pattern.text = text.clone();
                app.pattern.synced = Some((text, seed, cycle));
            }
        }
        // Rows stay on the grid when their last hit goes, or an undo takes
        // it back, though the text has no place for them; × removes them.
        let rows = &mut app.pattern.grid_rows;
        rows.retain(|(id, n)| *id != clip.id || g.rows.iter().any(|r| r.name == *n));
        for r in &g.rows {
            if r.hits.iter().all(Option::is_none)
                && !rows.iter().any(|(id, n)| *id == clip.id && *n == r.name)
            {
                rows.push((clip.id, r.name.clone()));
            }
        }
    }
}

/// A number as a pattern writes it: at most three decimals, no trailing
/// zeros.
pub fn format_number(value: f64) -> String {
    let text = format!("{value:.3}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" {
        "0".into()
    } else {
        text.into()
    }
}

/// What a control sets, for its row: a label and a tooltip.
fn control_label(track: &Track, name: &str) -> (String, String, bool) {
    if name == VELOCITY {
        return (
            "Velocity".into(),
            "How hard each note plays, 1 to 127; 0 skips the note".into(),
            true,
        );
    }
    let spec = track
        .instrument
        .as_ref()
        .and_then(|i| sky_dsp::params::find(i.kind.params(), name));
    match spec {
        Some(s) => (
            s.label.into(),
            format!(
                "{}: {} to {}{}",
                s.name,
                s.min,
                s.max,
                if s.unit.is_empty() {
                    String::new()
                } else {
                    format!(" {}", s.unit)
                }
            ),
            true,
        ),
        None => (
            name.into(),
            "This instrument has no such parameter, so the pattern does nothing".into(),
            false,
        ),
    }
}

/// The pattern's control rows and swing.
fn groove_ui(
    app: &mut App,
    ui: &mut Ui,
    track: &Track,
    clip: &Clip,
    controls: &BTreeMap<String, String>,
    swing: Swing,
) {
    let focused = controls.keys().any(|name| {
        ui.ctx()
            .memory(|m| m.has_focus(Id::new(("control_text", clip.id, name.as_str()))))
    });
    let st = &mut app.pattern;
    let stale = st
        .controls_synced
        .as_ref()
        .is_none_or(|(id, synced)| *id != clip.id || (!focused && synced != controls));
    if stale {
        st.controls = controls.clone().into_iter().collect();
        st.controls_synced = Some((clip.id, controls.clone()));
    }

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Controls").color(theme::TEXT_WEAK));
        ui.label(
            RichText::new("number patterns read at the start of each note")
                .small()
                .color(theme::TEXT_DIM),
        );
    });
    let mut commits: Vec<(String, Option<String>)> = Vec::new();
    egui::Grid::new(("pattern_controls", clip.id))
        .num_columns(3)
        .spacing([8.0, 4.0])
        .show(ui, |ui| {
            for (name, current) in controls {
                let (label, tip, known) = control_label(track, name);
                let color = if known { theme::TEXT } else { theme::TEXT_DIM };
                ui.label(RichText::new(label).color(color))
                    .on_hover_text(tip);
                let text = app
                    .pattern
                    .controls
                    .entry(name.clone())
                    .or_insert_with(|| current.clone());
                let resp = ui.add(
                    TextEdit::singleline(text)
                        .id(Id::new(("control_text", clip.id, name.as_str())))
                        .font(theme::mono(14.0))
                        .desired_width(280.0)
                        .hint_text("100 60 80 60"),
                );
                let parsed = sky_pattern::parse_numbers(text);
                if resp.lost_focus() && text != current {
                    match &parsed {
                        Ok(_) => commits.push((name.clone(), Some(text.clone()))),
                        Err(e) => app.error(format!(
                            "{name} not applied: {} at column {}",
                            e.message, e.column
                        )),
                    }
                }
                ui.horizontal(|ui| {
                    if ui
                        .small_button("×")
                        .on_hover_text("Remove this control")
                        .clicked()
                    {
                        commits.push((name.clone(), None));
                    }
                    if let Err(e) = &parsed {
                        ui.label(
                            RichText::new(format!("Column {}: {}", e.column, e.message))
                                .small()
                                .color(theme::ERROR),
                        );
                    }
                });
                ui.end_row();
            }
        });
    // Velocity, then the instrument's parameters, that are not used yet.
    let mut options: Vec<(String, String)> = Vec::new();
    if !controls.contains_key(VELOCITY) {
        options.push((VELOCITY.into(), "Velocity".into()));
    }
    if let Some(slot) = &track.instrument {
        for spec in slot.kind.params() {
            if !controls.contains_key(spec.name) {
                options.push((spec.name.into(), spec.label.into()));
            }
        }
    }
    ui.horizontal(|ui| {
        if controls.len() < sky_model::command::MAX_CONTROLS && !options.is_empty() {
            let mut picked: Option<String> = None;
            ComboBox::from_id_salt(("add_control", clip.id))
                .selected_text("Add control…")
                .width(150.0)
                .height(320.0)
                .show_ui(ui, |ui| {
                    for (name, label) in &options {
                        if ui.selectable_label(false, label).clicked() {
                            picked = Some(name.clone());
                        }
                    }
                });
            if let Some(name) = picked {
                // Start from the track's own value, so nothing changes yet.
                let start = match track.instrument.as_ref().and_then(|i| i.param(&name)) {
                    Some(v) => format_number(v as f64),
                    None => "100".into(),
                };
                commits.push((name, Some(start)));
            }
        }

        ui.separator();
        ui.label(RichText::new("Swing").color(theme::TEXT_WEAK));
        let mut amount = swing.amount;
        let r1 = ui
            .add(
                DragValue::new(&mut amount)
                    .range(Swing::STRAIGHT..=Swing::MAX)
                    .speed(0.2)
                    .suffix(" %"),
            )
            .on_hover_text(
                "Where every second step lands within its pair: 50 is straight, \
                 about 66 a triplet feel",
            );
        let mut grid = swing.grid;
        ComboBox::from_id_salt(("swing_grid", clip.id))
            .selected_text(format!("1/{grid}"))
            .width(60.0)
            .show_ui(ui, |ui| {
                for g in Swing::GRIDS {
                    ui.selectable_value(&mut grid, g, format!("1/{g}"));
                }
            });
        let next = Swing::new(amount, grid);
        if next != swing {
            let key = r1.changed().then(|| app.gestures.key(r1.id));
            let r = actions::set_swing(&app.session, clip.id, next, key);
            app.report(r);
        }
    });
    for (name, source) in commits {
        let r = actions::set_pattern_control(&app.session, clip.id, &name, source.as_deref());
        if app.report(r).is_some() {
            match source {
                Some(s) => {
                    app.pattern.controls.insert(name, s);
                }
                None => {
                    app.pattern.controls.remove(&name);
                }
            }
        }
    }
}

/// A strip showing the events of the first few cycles, and the playhead
/// while it is in them.
fn pattern_preview(ui: &mut Ui, text: &str, seed: u64, cycles: i64, at: Option<f64>) {
    let Ok(pattern) = sky_pattern::parse(text) else {
        return;
    };
    let cycles = cycles.clamp(1, 4);
    let width = ui.available_width().max(100.0);
    let (rect, _) = ui.allocate_exact_size(vec2(width, 54.0), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(3), theme::LANE);
    let cw = rect.width() / cycles as f32;
    let mut notes: Vec<(f32, f32, u8, String, bool)> = Vec::new();
    for c in 0..cycles {
        let x0 = rect.left() + c as f32 * cw;
        if c > 0 {
            painter.vline(x0, rect.y_range(), Stroke::new(1.0, theme::GRID_BAR));
        }
        painter.text(
            egui::pos2(x0 + 4.0, rect.top() + 2.0),
            Align2::LEFT_TOP,
            format!("cycle {}", c + 1),
            theme::small(),
            theme::TEXT_DIM,
        );
        for e in pattern
            .query(Time::from_integer(c), Time::from_integer(c + 1), seed)
            .into_iter()
            .take(2048)
        {
            let b = (*e.begin.numer() as f64 / (*e.begin.denom()).max(1) as f64 - c as f64) as f32;
            let en = (*e.end.numer() as f64 / (*e.end.denom()).max(1) as f64 - c as f64) as f32;
            let label = match &e.value {
                sky_pattern::Value::Sound { name, .. } => match e.controls.get(Control::Note) {
                    Some(_) => format!(
                        "{name} {}",
                        sky_core::music::note_name(e.note().unwrap_or(60))
                    ),
                    None => name.clone(),
                },
                sky_pattern::Value::Note(n) => sky_core::music::note_name(*n),
                sky_pattern::Value::Number(_) => format_number(e.value.number().unwrap_or(0.0)),
            };
            let sounding = at.is_some_and(|a| {
                live_pattern::cycles(e.begin) <= a && a < live_pattern::cycles(e.end)
            });
            notes.push((
                x0 + b * cw,
                x0 + en.min(1.0) * cw,
                e.note().unwrap_or(60),
                label,
                sounding,
            ));
        }
    }
    let lanes: Vec<u8> = {
        let mut v: Vec<u8> = notes.iter().map(|n| n.2).collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    let rows = lanes.len().max(1) as f32;
    let top = rect.top() + 16.0;
    let h = ((rect.bottom() - 3.0 - top) / rows).max(2.0);
    for (x0, x1, note, label, sounding) in notes {
        let row = lanes.iter().position(|n| *n == note).unwrap_or(0) as f32;
        let y = rect.bottom() - 3.0 - (row + 1.0) * h;
        let r = egui::Rect::from_min_max(
            egui::pos2(x0 + 1.0, y + 1.0),
            egui::pos2((x1 - 1.0).max(x0 + 3.0), y + h - 1.0),
        );
        let fill = if sounding {
            theme::lighten(theme::ACCENT, 0.45)
        } else {
            theme::ACCENT.gamma_multiply(0.8)
        };
        painter.rect_filled(r, CornerRadius::same(2), fill);
        if r.width() > 22.0 && h >= 9.0 {
            painter.with_clip_rect(r).text(
                egui::pos2(r.left() + 2.0, r.center().y),
                Align2::LEFT_CENTER,
                label,
                egui::FontId::proportional((h - 2.0).min(10.0)),
                Color32::BLACK,
            );
        }
    }
    if let Some(a) = at.filter(|a| *a < cycles as f64) {
        let x = rect.left() + a as f32 * cw;
        painter.vline(x, rect.y_range(), Stroke::new(1.5, theme::TEXT));
    }
}

#[allow(clippy::too_many_arguments)]
fn audio_ui(
    app: &mut App,
    ui: &mut Ui,
    project: &Project,
    clip: &Clip,
    source: sky_core::AudioId,
    gain_db: f32,
    fade_in: Ticks,
    fade_out: Ticks,
) {
    match project.pool_entry(source) {
        Ok(e) => {
            ui.label(
                RichText::new(format!(
                    "{} · {} ch · {} Hz · {:.2} s",
                    e.name,
                    e.channels,
                    e.sample_rate,
                    e.frames as f64 / e.sample_rate.max(1) as f64
                ))
                .color(theme::TEXT_WEAK),
            );
        }
        Err(e) => {
            ui.label(RichText::new(e.to_string()).color(theme::ERROR));
        }
    }
    let beats_len = clip.length as f64 / PPQ as f64;
    egui::Grid::new(("audio_clip", clip.id))
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Gain");
            let mut g = gain_db;
            let resp = ui.add(
                Slider::new(&mut g, -60.0..=24.0)
                    .suffix(" dB")
                    .max_decimals(1),
            );
            if resp.changed() && g != gain_db {
                let key = app.gestures.key(resp.id);
                let r = actions::set_audio_clip(
                    &app.session,
                    clip.id,
                    AudioClipChange {
                        gain_db: Some(g),
                        ..Default::default()
                    },
                    Some(key),
                );
                app.report(r);
            }
            ui.end_row();
            for (label, value, is_in) in [("Fade in", fade_in, true), ("Fade out", fade_out, false)]
            {
                ui.label(label);
                let mut beats = value as f64 / PPQ as f64;
                let resp = ui.add(
                    DragValue::new(&mut beats)
                        .range(0.0..=beats_len)
                        .speed(0.02)
                        .max_decimals(2)
                        .suffix(" beats"),
                );
                if resp.changed() {
                    let ticks = (beats * PPQ as f64).round() as Ticks;
                    if ticks != value {
                        let key = app.gestures.key(resp.id);
                        let change = if is_in {
                            AudioClipChange {
                                fade_in: Some(ticks),
                                ..Default::default()
                            }
                        } else {
                            AudioClipChange {
                                fade_out: Some(ticks),
                                ..Default::default()
                            }
                        };
                        let r = actions::set_audio_clip(&app.session, clip.id, change, Some(key));
                        app.report(r);
                    }
                }
                ui.end_row();
            }
        });
}
