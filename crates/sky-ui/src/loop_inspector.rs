// SPDX-License-Identifier: GPL-3.0-or-later
//! The Loop inspector: a game loop, or the loop region, rendered the way it
//! exports, with the waveform on both sides of its seam, the seam check, and
//! Play and Join to hear it. It renders again in the background whenever the
//! project changes while it is in view.

use std::sync::Arc;

use egui::{
    Align2, Color32, ComboBox, Context, CornerRadius, Painter, Rect, RichText, Sense, Shape,
    Stroke, Ui, pos2, vec2,
};
use sky_core::AssetId;
use sky_dsp::AudioData;
use sky_dsp::analysis::{SeamReport, seam_check};
use sky_model::{GameAssetKind, Project};
use sky_session::Session;

use crate::app::App;
use crate::audition::{PreviewKey, preview_button};
use crate::jobs::JobOutput;
use crate::waveform::Peaks;
use crate::{game, theme};

/// What the inspector shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    /// The transport's loop region, as Export loop renders it.
    Region,
    /// A loop of the game assets.
    Asset(AssetId),
}

/// A loop rendered as it exports, and how its end joins its start.
pub struct Checked {
    pub target: Target,
    /// The project revision it was rendered from.
    pub revision: u64,
    pub audio: Arc<AudioData>,
    /// The frame the body starts at, after any intro.
    pub loop_start: usize,
    pub seam: SeamReport,
    peaks: Peaks,
}

/// How much time around the seam the waveform shows, in seconds.
pub const SPANS: [(&str, f64); 5] = [
    ("5 ms", 0.005),
    ("20 ms", 0.02),
    ("100 ms", 0.1),
    ("500 ms", 0.5),
    ("2 s", 2.0),
];

pub struct InspectorState {
    /// What was picked last. Without a pick, or once the pick is gone, it
    /// shows the first loop, or the loop region when there is none.
    pub target: Option<Target>,
    /// An index into [`SPANS`].
    pub span: usize,
    pub checked: Option<Arc<Checked>>,
    failed: Option<(Target, u64, String)>,
    /// The render running now.
    pending: Option<(Target, u64)>,
}

impl Default for InspectorState {
    fn default() -> Self {
        Self {
            target: None,
            span: 2,
            checked: None,
            failed: None,
            pending: None,
        }
    }
}

impl InspectorState {
    /// Forgets the old project's loops, keeping the zoom.
    pub fn reset(&mut self) {
        *self = Self {
            span: self.span,
            ..Self::default()
        };
    }

    pub fn is_rendering(&self) -> bool {
        self.pending.is_some()
    }
}

fn is_loop(project: &Project, id: AssetId) -> bool {
    matches!(
        project.game.asset(id).map(|a| &a.kind),
        Some(GameAssetKind::Loop { .. })
    )
}

/// Every loop the inspector can show: the loop region, then the game's
/// loops.
fn targets(project: &Project) -> Vec<Target> {
    let mut out = vec![Target::Region];
    out.extend(
        project
            .game
            .assets
            .iter()
            .filter(|a| matches!(a.kind, GameAssetKind::Loop { .. }))
            .map(|a| Target::Asset(a.id)),
    );
    out
}

fn name(project: &Project, target: Target) -> String {
    match target {
        Target::Region => format!(
            "The loop region, {}",
            game::bars(project, project.loop_region)
        ),
        Target::Asset(id) => match project.game.asset(id) {
            Some(a) => match &a.kind {
                GameAssetKind::Loop { range, .. } => {
                    format!("{}, {}", a.name, game::bars(project, *range))
                }
                GameAssetKind::Sfx { .. } => a.name.clone(),
            },
            None => "A removed loop".to_string(),
        },
    }
}

/// Renders a loop the way it exports and checks its seam.
fn check(
    session: &Session,
    project: &Project,
    target: Target,
    revision: u64,
) -> Result<Checked, String> {
    let (audio, loop_start, seam) = match target {
        Target::Region => {
            let audio = session.render_loop(None).map_err(|e| e.to_string())?;
            let seam = seam_check(&audio, 0, audio.frames());
            (audio, 0, seam)
        }
        Target::Asset(id) => {
            let name = project
                .game
                .asset(id)
                .map(|a| a.name.clone())
                .ok_or_else(|| "That loop is no longer in the project".to_string())?;
            let r = session
                .render_game_asset(&name)
                .map_err(|e| e.to_string())?;
            let audio = r
                .audio
                .into_iter()
                .next()
                .ok_or_else(|| "The loop rendered no audio".to_string())?;
            let seam = r
                .seam
                .unwrap_or_else(|| seam_check(&audio, r.loop_start, audio.frames()));
            (audio, r.loop_start, seam)
        }
    };
    let peaks = Peaks::compute(&audio);
    Ok(Checked {
        target,
        revision,
        audio: Arc::new(audio),
        loop_start,
        seam,
        peaks,
    })
}

impl App {
    /// What the inspector shows now.
    pub fn inspected(&self) -> Target {
        let project = &self.view.project;
        match self.inspector.target {
            Some(Target::Region) => Target::Region,
            Some(Target::Asset(id)) if is_loop(project, id) => Target::Asset(id),
            _ => targets(project).get(1).copied().unwrap_or(Target::Region),
        }
    }

    /// Shows a game loop, or the loop region, in the inspector and brings
    /// the inspector to the front.
    pub fn inspect(&mut self, target: Target) {
        self.inspector.target = Some(target);
        self.workspace.show(crate::app::Tab::Loop);
    }

    /// Renders what the inspector shows when it has not been rendered from
    /// the project as it is now: one render at a time, and not while a
    /// button is held, so a drag renders once, at its end.
    fn check_seam_when_due(&mut self, ctx: &Context) {
        let target = self.inspected();
        let revision = self.view.revision;
        let st = &self.inspector;
        let done = st
            .checked
            .as_ref()
            .is_some_and(|c| c.target == target && c.revision == revision)
            || st
                .failed
                .as_ref()
                .is_some_and(|(t, r, _)| *t == target && *r == revision);
        if done || st.pending.is_some() || ctx.input(|i| i.pointer.any_down()) {
            return;
        }
        self.inspector.pending = Some((target, revision));
        let session = self.session.clone();
        let project = self.view.project.clone();
        self.jobs.spawn(ctx, None, move || JobOutput::SeamChecked {
            target,
            revision,
            result: check(&session, &project, target, revision).map(Arc::new),
        });
    }

    /// A render for the inspector finished.
    pub fn seam_checked(
        &mut self,
        target: Target,
        revision: u64,
        result: Result<Arc<Checked>, String>,
    ) {
        if self.inspector.pending != Some((target, revision)) {
            // From a project that has since been replaced.
            return;
        }
        self.inspector.pending = None;
        match result {
            Ok(c) => {
                self.inspector.checked = Some(c);
                self.inspector.failed = None;
            }
            Err(e) => self.inspector.failed = Some((target, revision, e)),
        }
    }
}

/// The inspector's tab.
pub fn show(app: &mut App, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    app.check_seam_when_due(&ctx);
    let project = app.view.project.clone();
    let target = app.inspected();
    let play = PreviewKey::Inspected { join: false };
    let join = PreviewKey::Inspected { join: true };
    let (play_state, join_state) = (app.preview_state(&play), app.preview_state(&join));
    let checked = app.inspector.checked.clone().filter(|c| c.target == target);
    let failed = app
        .inspector
        .failed
        .as_ref()
        .filter(|(t, r, _)| *t == target && *r == app.view.revision)
        .map(|(_, _, e)| e.clone());
    let rendering = app.inspector.is_rendering();
    let mut pick = None;
    let mut preview = None;

    ui.horizontal(|ui| {
        ui.label(RichText::new("Loop").color(theme::TEXT_WEAK));
        ComboBox::from_id_salt("inspect_target")
            .width(300.0)
            .selected_text(name(&project, target))
            .show_ui(ui, |ui| {
                for t in targets(&project) {
                    if ui
                        .selectable_label(t == target, name(&project, t))
                        .clicked()
                    {
                        pick = Some(t);
                    }
                }
            });
        ui.add_enabled_ui(checked.is_some(), |ui| {
            if preview_button(
                ui,
                "Play",
                "Play it as the game will: the intro once, then the body repeating until stopped",
                play_state,
            ) {
                preview = Some(play.clone());
            }
            if preview_button(
                ui,
                "Join",
                "Play from just before the body ends, to hear it wrap around to its start",
                join_state,
            ) {
                preview = Some(join.clone());
            }
        });
        if rendering {
            ui.spinner();
            ui.label(RichText::new("Rendering…").small().color(theme::TEXT_WEAK));
        }
    });

    match (&checked, failed) {
        (_, Some(e)) => {
            ui.label(RichText::new(format!("Cannot render it: {e}")).color(theme::ERROR));
        }
        (Some(c), None) => {
            report(ui, c, c.revision != app.view.revision);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Around the seam").color(theme::TEXT_WEAK));
                for (i, (label, _)) in SPANS.iter().enumerate() {
                    ui.selectable_value(&mut app.inspector.span, i, *label);
                }
            });
            let seconds = SPANS[app.inspector.span.min(SPANS.len() - 1)].1;
            let avail = ui.available_size();
            let overview_h = 40.0;
            let wave_h = (avail.y - overview_h - 10.0).max(80.0);
            let (rect, _) = ui.allocate_exact_size(vec2(avail.x, wave_h), Sense::hover());
            draw_seam(&ui.painter_at(rect), rect, c, seconds);
            ui.add_space(4.0);
            let (rect, _) = ui.allocate_exact_size(vec2(avail.x, overview_h), Sense::hover());
            draw_overview(&ui.painter_at(rect), rect, c);
        }
        (None, None) => {
            ui.add_space(8.0);
            ui.label(
                RichText::new(format!(
                    "Rendering {} the way it exports, to check its seam…",
                    name(&project, target)
                ))
                .color(theme::TEXT_WEAK),
            );
        }
    }

    if let Some(t) = pick {
        app.inspector.target = Some(t);
        app.stop_preview();
    }
    if let Some(key) = preview {
        app.toggle_preview(&ctx, key);
    }
}

fn seam_color(seam: &SeamReport) -> Color32 {
    if seam.verdict == "seamless" {
        theme::OK
    } else {
        theme::WARN
    }
}

/// The verdict and the numbers behind it, and how long the loop is.
fn report(ui: &mut Ui, c: &Checked, stale: bool) {
    let s = &c.seam;
    ui.horizontal_wrapped(|ui| {
        let verdict = if s.verdict == "seamless" {
            "Seamless"
        } else {
            "A click at the seam"
        };
        ui.label(RichText::new(verdict).strong().color(seam_color(s)));
        ui.label(
            RichText::new(format!(
                "The end jumps {:.3} to the start, where neighbouring samples step up to {:.3}. \
                 Level {:.1} dB before the seam, {:.1} dB after.",
                s.sample_jump, s.typical_step, s.level_before_db, s.level_after_db
            ))
            .small()
            .color(theme::TEXT_WEAK),
        );
    });
    if let Some(note) = s.note {
        ui.label(RichText::new(note).small().color(theme::TEXT_WEAK));
    }
    let rate = f64::from(c.audio.sample_rate.max(1));
    let body = c.audio.frames().saturating_sub(c.loop_start) as f64 / rate;
    let mut length = format!("Body {body:.2} s");
    if c.loop_start > 0 {
        length += &format!(", intro {:.2} s before it", c.loop_start as f64 / rate);
    }
    if c.target == Target::Region {
        length += ", rendered twice so tails from its end wrap into its start";
    }
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(length).small().color(theme::TEXT_DIM));
        if stale {
            ui.label(
                RichText::new("The project changed since; this is the last render")
                    .small()
                    .color(theme::WARN),
            );
        }
    });
}

/// The sample at `offset` frames from the seam: before it, the end of the
/// body; from it on, the body's start.
fn around(c: &Checked, ch: &[f32], offset: isize) -> f32 {
    let frames = ch.len() as isize;
    let i = if offset < 0 {
        frames + offset
    } else {
        c.loop_start as isize + offset
    };
    usize::try_from(i)
        .ok()
        .and_then(|i| ch.get(i))
        .copied()
        .unwrap_or(0.0)
}

/// The waveform on both sides of the seam, one lane per channel: the end of
/// the body left of the line, its start right of it.
fn draw_seam(painter: &Painter, rect: Rect, c: &Checked, seconds: f64) {
    painter.rect_filled(rect, CornerRadius::same(3), theme::BG);
    let frames = c.audio.frames();
    let body = frames.saturating_sub(c.loop_start);
    let seam = seam_color(&c.seam);
    if body < 4 || c.audio.channels.is_empty() {
        return;
    }
    let half =
        ((seconds * f64::from(c.audio.sample_rate) / 2.0).round() as usize).clamp(1, body / 2);
    let total = (2 * half) as f32;
    let width = rect.width().max(1.0);
    let per_px = total / width;
    let x_of = |offset: f32| rect.left() + (offset + half as f32) / total * width;
    let lanes = c.audio.channels.len().min(2);
    let lane_h = rect.height() / lanes as f32;
    let wave = theme::GAME_LOOP;
    // Quiet loops are drawn taller, by a power of two, so a jump shows.
    let peak = c
        .audio
        .channels
        .iter()
        .take(lanes)
        .flat_map(|ch| (-(half as isize)..half as isize).map(move |o| around(c, ch, o).abs()))
        .fold(0.0f32, f32::max);
    let gain = 2f32.powi((0.9 / peak.max(1e-6)).clamp(1.0, 16.0).log2().floor() as i32);
    for (k, ch) in c.audio.channels.iter().take(lanes).enumerate() {
        let lane = Rect::from_min_size(
            pos2(rect.left(), rect.top() + k as f32 * lane_h),
            vec2(rect.width(), lane_h),
        );
        let (cy, amp) = (lane.center().y, lane.height() * 0.45);
        painter.hline(lane.x_range(), cy, Stroke::new(1.0, theme::GRID_BEAT));
        let y = |s: f32| cy - (s * gain).clamp(-1.0, 1.0) * amp;
        if per_px <= 8.0 {
            // Few enough samples to join each one to the next.
            let before: Vec<_> = (-(half as isize)..0)
                .map(|o| pos2(x_of(o as f32), y(around(c, ch, o))))
                .collect();
            let after: Vec<_> = (0..half as isize)
                .map(|o| pos2(x_of(o as f32), y(around(c, ch, o))))
                .collect();
            if let (Some(a), Some(b)) = (before.last(), after.first()) {
                painter.line_segment([*a, *b], Stroke::new(1.5, seam));
            }
            painter.add(Shape::line(before, Stroke::new(1.25, wave)));
            painter.add(Shape::line(after, Stroke::new(1.25, wave)));
        } else {
            let mut lines = Vec::with_capacity(width as usize);
            for px in 0..width as usize {
                let a = (px as f32 * per_px) as isize - half as isize;
                let b = (((px + 1) as f32 * per_px) as isize - half as isize).max(a + 1);
                // From the last sample of the column before, so they join.
                let from = if px == 0 { a } else { a - 1 };
                let (lo, hi) = (from..b).fold((f32::MAX, f32::MIN), |(lo, hi), o| {
                    let s = around(c, ch, o);
                    (lo.min(s), hi.max(s))
                });
                let x = rect.left() + px as f32 + 0.5;
                let (top, bottom) = (y(hi), y(lo).max(y(hi) + 1.5));
                lines.push(Shape::vline(x, top..=bottom, Stroke::new(1.0, wave)));
            }
            painter.extend(lines);
        }
    }
    let mid = x_of(0.0);
    painter.vline(mid, rect.y_range(), Stroke::new(1.5, seam));
    let label = |pos, align, text: &str| {
        painter.text(pos, align, text, theme::small(), theme::TEXT_WEAK);
    };
    label(
        rect.left_top() + vec2(6.0, 4.0),
        Align2::LEFT_TOP,
        "End of the loop",
    );
    label(
        rect.right_top() + vec2(-6.0, 4.0),
        Align2::RIGHT_TOP,
        "Start of the loop",
    );
    let ms = half as f64 / f64::from(c.audio.sample_rate) * 1000.0;
    let mut span = if ms >= 1000.0 {
        format!("{:.1} s each side", ms / 1000.0)
    } else if ms < 10.0 {
        format!("{ms:.1} ms each side")
    } else {
        format!("{ms:.0} ms each side")
    };
    if gain > 1.0 {
        span += &format!(", drawn {gain:.0} times taller");
    }
    painter.text(
        pos2(mid + 6.0, rect.bottom() - 4.0),
        Align2::LEFT_BOTTOM,
        span,
        theme::small(),
        theme::TEXT_DIM,
    );
}

/// The whole render: the intro shaded, then the body up to the seam.
fn draw_overview(painter: &Painter, rect: Rect, c: &Checked) {
    painter.rect_filled(rect, CornerRadius::same(3), theme::BG);
    let frames = c.audio.frames().max(1);
    let x_of = |f: usize| rect.left() + f as f32 / frames as f32 * rect.width();
    if c.loop_start > 0 {
        let intro = Rect::from_min_max(rect.min, pos2(x_of(c.loop_start), rect.bottom()));
        painter.rect_filled(
            intro,
            CornerRadius::same(3),
            theme::GAME_LOOP.gamma_multiply(0.12),
        );
        painter.text(
            intro.left_top() + vec2(4.0, 2.0),
            Align2::LEFT_TOP,
            "intro",
            theme::small(),
            theme::TEXT_DIM,
        );
        painter.vline(
            x_of(c.loop_start),
            rect.y_range(),
            Stroke::new(1.0, theme::GAME_LOOP),
        );
    }
    let (cy, amp) = (rect.center().y, rect.height() * 0.45);
    let per_px = frames as f64 / f64::from(rect.width().max(1.0));
    let mut lines = Vec::new();
    for px in 0..rect.width() as usize {
        let from = px as f64 * per_px;
        if let Some((lo, hi)) = c.peaks.range(from, from + per_px) {
            let x = rect.left() + px as f32 + 0.5;
            let top = cy - hi.clamp(-1.0, 1.0) * amp;
            let bottom = (cy - lo.clamp(-1.0, 1.0) * amp).max(top + 1.0);
            lines.push(Shape::vline(
                x,
                top..=bottom,
                Stroke::new(1.0, theme::darken(theme::GAME_LOOP, 0.2)),
            ));
        }
    }
    painter.extend(lines);
    painter.vline(
        rect.right() - 1.0,
        rect.y_range(),
        Stroke::new(2.0, seam_color(&c.seam)),
    );
}
