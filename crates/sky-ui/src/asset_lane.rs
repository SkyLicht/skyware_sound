// SPDX-License-Identifier: GPL-3.0-or-later
//! The game asset lane under the bar ruler: each loop (its intro and its
//! body) and each SFX set (one slice per variation) as a colored region.
//! Dragging a region moves it, dragging an edge changes it, and each drag is
//! one undo step.

use egui::{
    Align, Align2, Button, Color32, Context, CornerRadius, CursorIcon, Id, Key, Layout, Painter,
    Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Ui, UiBuilder, pos2,
};
use sky_core::{AssetId, PPQ, Ticks};
use sky_model::{GameAsset, GameAssetKind, Project, TimeRange};

use crate::app::{App, Tab};
use crate::arrange::{ArrangeState, snap_round};
use crate::loop_inspector::Target;
use crate::{actions, game, theme};

/// Height of one row of regions.
pub const ROW: f32 = 18.0;
/// Assets that overlap go in rows of their own, up to this many.
const MAX_ROWS: usize = 6;
/// How near an edge, in points, the pointer grabs it.
const EDGE: f32 = 5.0;
/// Regions narrower than this move as a whole; their edges are too close.
const MIN_EDGES_W: f32 = 16.0;

/// What a drag on a region changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// The whole asset.
    Move,
    /// Where a loop's intro starts. Dragged onto the body, the intro goes.
    Intro,
    /// Where a loop's body starts.
    Start,
    /// Where a loop's body ends: its seam.
    End,
    /// Every SFX variation edge at this tick: a boundary between two
    /// variations that touch moves for both.
    Edge(Ticks),
}

#[derive(Clone, Debug)]
pub struct AssetDrag {
    pub asset: AssetId,
    pub part: Part,
    pub grab_tick: f64,
    pub orig: GameAsset,
    pub preview: GameAsset,
}

/// What a region asked for in one frame, done once the lane is drawn.
enum Wanted {
    Select(AssetId),
    Inspect(AssetId),
    Window,
    Update(GameAsset),
    Remove(AssetId),
    LoopRegion(TimeRange),
}

/// The ticks an asset covers: a loop from its intro to its seam, an SFX
/// set from its first variation to its last.
pub fn span(asset: &GameAsset) -> TimeRange {
    match &asset.kind {
        GameAssetKind::Loop { range, intro_start } => TimeRange {
            start: intro_start.unwrap_or(range.start),
            end: range.end,
        },
        GameAssetKind::Sfx { variations, .. } => {
            TimeRange::covering(variations).unwrap_or(TimeRange { start: 0, end: 1 })
        }
    }
}

/// The row of each asset, in project order: an asset takes the first row
/// where it overlaps nothing.
pub fn rows(project: &Project) -> Vec<(AssetId, usize)> {
    let mut taken: Vec<Vec<TimeRange>> = Vec::new();
    project
        .game
        .assets
        .iter()
        .map(|a| {
            let s = span(a);
            let free = taken
                .iter()
                .position(|row| row.iter().all(|r| r.end <= s.start || s.end <= r.start));
            let row = match free {
                Some(r) => r,
                None if taken.len() < MAX_ROWS => {
                    taken.push(Vec::new());
                    taken.len() - 1
                }
                None => MAX_ROWS - 1,
            };
            taken[row].push(s);
            (a.id, row)
        })
        .collect()
}

/// How tall the lane is: nothing without game assets.
pub fn height(project: &Project) -> f32 {
    let rows = rows(project).iter().map(|(_, r)| r + 1).max().unwrap_or(0);
    if rows == 0 {
        0.0
    } else {
        rows as f32 * ROW + 4.0
    }
}

fn color(asset: &GameAsset) -> Color32 {
    match asset.kind {
        GameAssetKind::Loop { .. } => theme::GAME_LOOP,
        GameAssetKind::Sfx { .. } => theme::GAME_SFX,
    }
}

/// The edges a drag can grab, with the part each one changes.
fn edges(asset: &GameAsset) -> Vec<(Ticks, Part)> {
    match &asset.kind {
        GameAssetKind::Loop { range, intro_start } => {
            let mut out = Vec::new();
            if let Some(i) = intro_start {
                out.push((*i, Part::Intro));
            }
            out.push((range.start, Part::Start));
            out.push((range.end, Part::End));
            out
        }
        GameAssetKind::Sfx { variations, .. } => {
            let mut ticks: Vec<Ticks> = variations.iter().flat_map(|v| [v.start, v.end]).collect();
            ticks.sort_unstable();
            ticks.dedup();
            ticks.into_iter().map(|t| (t, Part::Edge(t))).collect()
        }
    }
}

/// The asset as `part` dragged by `delta` ticks would leave it, kept valid:
/// nothing before the start of the song or shorter than `min_len`.
fn dragged(
    orig: &GameAsset,
    part: Part,
    tick: f64,
    grab_tick: f64,
    grid: Option<Ticks>,
    min_len: Ticks,
) -> GameAsset {
    let mut out = orig.clone();
    let delta = tick - grab_tick;
    match (&mut out.kind, part) {
        (kind, Part::Move) => {
            let first = span(orig).start;
            let dt = (snap_round(first as f64 + delta, grid) - first).max(-first);
            match kind {
                GameAssetKind::Loop { range, intro_start } => {
                    range.start += dt;
                    range.end += dt;
                    if let Some(i) = intro_start {
                        *i += dt;
                    }
                }
                GameAssetKind::Sfx { variations, .. } => {
                    for v in variations {
                        v.start += dt;
                        v.end += dt;
                    }
                }
            }
        }
        (GameAssetKind::Loop { range, intro_start }, Part::Intro) => {
            let t = snap_round(tick, grid).max(0);
            *intro_start = (t < range.start).then_some(t);
        }
        (GameAssetKind::Loop { range, intro_start }, Part::Start) => {
            let lo = intro_start.map_or(0, |i| i + min_len);
            let hi = range.end - min_len;
            // A body already too short for the grid keeps its start.
            if lo <= hi {
                range.start = snap_round(tick, grid).clamp(lo, hi);
            }
        }
        (GameAssetKind::Loop { range, .. }, Part::End) => {
            range.end = snap_round(tick, grid).max(range.start + min_len);
        }
        (GameAssetKind::Sfx { variations, .. }, Part::Edge(at)) => {
            let ends = variations.iter().filter(|v| v.end == at);
            let starts = variations.iter().filter(|v| v.start == at);
            let lo = ends.map(|v| v.start + min_len).max().unwrap_or(0).max(0);
            let hi = starts.map(|v| v.end - min_len).min().unwrap_or(Ticks::MAX);
            if lo > hi {
                return out;
            }
            let t = snap_round(tick, grid).clamp(lo, hi);
            for v in variations.iter_mut() {
                if v.start == at {
                    v.start = t;
                }
                if v.end == at {
                    v.end = t;
                }
            }
        }
        _ => {}
    }
    out
}

/// The snap grid of a drag, off while Alt is held, and the shortest length
/// it leaves.
fn grid(app: &App, ctx: &Context, project: &Project) -> (Option<Ticks>, Ticks) {
    let alt = ctx.input(|i| i.modifiers.alt);
    let grid = if alt {
        None
    } else {
        app.arrange.snap.grid(project.time_signature)
    };
    (grid, grid.unwrap_or(PPQ / 4).max(1))
}

/// Follows a region drag; applies it as one change when the button comes
/// up, or drops it on Escape.
pub fn update_drag(app: &mut App, ctx: &Context, project: &Project) {
    let Some(mut d) = app.arrange.asset_drag.take() else {
        return;
    };
    if project.game.asset(d.asset).is_none() {
        return;
    }
    let (pos, down, escape) = ctx.input(|i| {
        (
            i.pointer.latest_pos(),
            i.pointer.primary_down(),
            i.key_pressed(Key::Escape),
        )
    });
    if escape {
        return;
    }
    let (grid, min_len) = grid(app, ctx, project);
    if let Some(p) = pos {
        let tick = app.arrange.tick_at(p.x);
        d.preview = dragged(&d.orig, d.part, tick, d.grab_tick, grid, min_len);
        ctx.set_cursor_icon(match d.part {
            Part::Move => CursorIcon::Grabbing,
            _ => CursorIcon::ResizeHorizontal,
        });
    }
    if down {
        app.arrange.asset_drag = Some(d);
        return;
    }
    if d.preview != d.orig {
        let r = game::update(&app.session, d.preview, None);
        app.report(r);
        app.refresh();
    }
}

/// The part of a region under `x`: the nearest edge within reach, or the
/// whole region.
fn part_at(st: &ArrangeState, asset: &GameAsset, rect: Rect, x: f32) -> Part {
    if rect.width() < MIN_EDGES_W {
        return Part::Move;
    }
    edges(asset)
        .into_iter()
        .map(|(t, part)| ((st.x_of(t as f64) - x).abs(), part))
        .filter(|(d, _)| *d <= EDGE)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map_or(Part::Move, |(_, p)| p)
}

/// The rectangle of an asset's region in row `row` of the lane.
fn region_rect(st: &ArrangeState, lane: Rect, asset: &GameAsset, row: usize) -> Rect {
    let s = span(asset);
    let top = lane.top() + 2.0 + row as f32 * ROW;
    Rect::from_min_max(
        pos2(st.x_of(s.start as f64), top),
        pos2(st.x_of(s.end as f64), top + ROW - 2.0),
    )
}

/// The id of an asset's region.
pub fn region_id(asset: AssetId) -> Id {
    Id::new(("game_asset_region", asset))
}

/// The lane and its corner. `rect` is the lane right of the track headers.
pub fn show(app: &mut App, ui: &mut Ui, corner: Rect, rect: Rect, project: &Project) {
    if rect.height() <= 0.0 {
        return;
    }
    corner_ui(app, ui, corner);
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::ZERO, theme::PANEL);
    crate::arrange::draw_grid(&painter, &app.arrange, rect, project);
    painter.hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, theme::BG),
    );

    // A click between regions picks nothing.
    if ui
        .interact(rect, Id::new("game_asset_lane"), Sense::click())
        .clicked()
    {
        app.arrange.asset = None;
    }
    let rows = rows(project);
    let selected = app.arrange.asset;
    let mut wanted = Vec::new();
    for asset in &project.game.assets {
        let row = rows
            .iter()
            .find(|(id, _)| *id == asset.id)
            .map_or(0, |(_, r)| *r);
        // A region being dragged shows where it would go.
        let shown = match &app.arrange.asset_drag {
            Some(d) if d.asset == asset.id => &d.preview,
            _ => asset,
        };
        let r = region_rect(&app.arrange, rect, shown, row);
        let lit = selected == Some(asset.id);
        draw_region(&painter, &app.arrange, shown, r, lit);
        // Only the part in view takes the pointer, so a lane cut short
        // leaves the tracks below alone.
        let hit = region_rect(&app.arrange, rect, asset, row).intersect(rect);
        if hit.is_positive() {
            region_ui(app, ui, project, asset, hit, &mut wanted);
        }
    }

    for w in wanted {
        match w {
            Wanted::Select(id) => select(app, project, id),
            Wanted::Inspect(id) => {
                select(app, project, id);
                app.workspace.show(Tab::Loop);
            }
            Wanted::Window => app.open_game_assets(),
            Wanted::Update(asset) => {
                let r = game::update(&app.session, asset, None);
                app.report(r);
            }
            Wanted::Remove(id) => {
                let r = game::remove(&app.session, id);
                app.report(r);
                app.arrange.asset = None;
            }
            Wanted::LoopRegion(range) => {
                let r = actions::set_loop_region(&app.session, range.start, range.end);
                app.report(r);
            }
        }
        app.refresh();
    }
}

/// Picks an asset on the timeline; a loop also goes into the inspector.
fn select(app: &mut App, project: &Project, id: AssetId) {
    app.arrange.asset = Some(id);
    if let Some(GameAssetKind::Loop { .. }) = project.game.asset(id).map(|a| &a.kind) {
        app.inspector.target = Some(Target::Asset(id));
    }
}

fn corner_ui(app: &mut App, ui: &mut Ui, corner: Rect) {
    let painter = ui.painter_at(corner);
    painter.rect_filled(corner, CornerRadius::ZERO, theme::HEADER);
    painter.hline(
        corner.x_range(),
        corner.bottom() - 0.5,
        Stroke::new(1.0, theme::BG),
    );
    let button = Rect::from_min_max(
        pos2(corner.left() + 4.0, corner.top() + 2.0),
        pos2(corner.right() - 4.0, corner.top() + 2.0 + ROW - 2.0),
    );
    let r = ui
        .scope_builder(
            UiBuilder::new()
                .max_rect(button)
                .layout(Layout::left_to_right(Align::Center)),
            |ui| {
                ui.add(
                    Button::new(
                        RichText::new("Game assets…")
                            .small()
                            .color(theme::TEXT_WEAK),
                    )
                    .frame(false),
                )
            },
        )
        .inner
        .on_hover_text(
            "Loops and SFX sets the game export writes. Drag a region to move it, an edge \
             to change it; double-click a loop to check its seam.",
        );
    if r.clicked() {
        app.open_game_assets();
    }
}

/// A region's hover text, clicks, drags and right-click menu.
fn region_ui(
    app: &mut App,
    ui: &mut Ui,
    project: &Project,
    asset: &GameAsset,
    rect: Rect,
    wanted: &mut Vec<Wanted>,
) {
    let resp = ui.interact(rect, region_id(asset.id), Sense::click_and_drag());
    let dragging = app.arrange.asset_drag.is_some();
    if let Some(h) = resp.hover_pos()
        && !dragging
    {
        ui.ctx()
            .set_cursor_icon(match part_at(&app.arrange, asset, rect, h.x) {
                Part::Move => CursorIcon::Grab,
                _ => CursorIcon::ResizeHorizontal,
            });
    }
    let resp = if dragging {
        resp
    } else {
        resp.on_hover_ui(|ui| tooltip(ui, project, asset))
    };
    if resp.drag_started()
        && let Some(origin) = ui.input(|i| i.pointer.press_origin())
    {
        let part = part_at(&app.arrange, asset, rect, origin.x);
        app.arrange.asset_drag = Some(AssetDrag {
            asset: asset.id,
            part,
            grab_tick: app.arrange.tick_at(origin.x),
            orig: asset.clone(),
            preview: asset.clone(),
        });
        wanted.push(Wanted::Select(asset.id));
    }
    if resp.double_clicked() {
        wanted.push(match asset.kind {
            GameAssetKind::Loop { .. } => Wanted::Inspect(asset.id),
            GameAssetKind::Sfx { .. } => Wanted::Window,
        });
    } else if resp.clicked() || resp.secondary_clicked() {
        wanted.push(Wanted::Select(asset.id));
    }
    resp.context_menu(|ui| menu(ui, project, asset, wanted));
}

fn tooltip(ui: &mut Ui, project: &Project, asset: &GameAsset) {
    ui.label(RichText::new(&asset.name).strong());
    match &asset.kind {
        GameAssetKind::Loop { range, intro_start } => {
            ui.label(format!("Loop, {}", game::bars(project, *range)));
            if let Some(i) = intro_start {
                let intro = TimeRange {
                    start: *i,
                    end: range.start,
                };
                ui.label(format!("Intro {}, played once", game::bars(project, intro)));
            }
            ui.label(
                RichText::new(
                    "Drag to move it, or drag an edge: the right one is the seam. \
                     Double-click to check the seam.",
                )
                .small()
                .color(theme::TEXT_WEAK),
            );
        }
        GameAssetKind::Sfx { variations, .. } => {
            let n = variations.len();
            ui.label(format!(
                "SFX set, {n} variation{}, {}",
                if n == 1 { "" } else { "s" },
                game::bars(project, span(asset))
            ));
            ui.label(
                RichText::new(
                    "Drag to move it, or drag an edge between variations. Double-click for \
                     its settings.",
                )
                .small()
                .color(theme::TEXT_WEAK),
            );
        }
    }
}

fn menu(ui: &mut Ui, project: &Project, asset: &GameAsset, wanted: &mut Vec<Wanted>) {
    let bar = project.ticks_per_bar().max(1);
    let region = project.loop_region;
    match &asset.kind {
        GameAssetKind::Loop { range, intro_start } => {
            if ui.button("Check the seam").clicked() {
                wanted.push(Wanted::Inspect(asset.id));
            }
            match intro_start {
                Some(_) => {
                    if ui.button("Remove the intro").clicked() {
                        let mut a = asset.clone();
                        a.kind = GameAssetKind::Loop {
                            range: *range,
                            intro_start: None,
                        };
                        wanted.push(Wanted::Update(a));
                    }
                }
                None => {
                    let r = ui
                        .add_enabled(range.start > 0, Button::new("Add a bar of intro"))
                        .on_disabled_hover_text(
                            "The loop starts at bar 1, so nothing comes before it",
                        );
                    if r.clicked() {
                        let mut a = asset.clone();
                        a.kind = GameAssetKind::Loop {
                            range: *range,
                            intro_start: Some((range.start - bar).max(0)),
                        };
                        wanted.push(Wanted::Update(a));
                    }
                }
            }
            ui.separator();
            if ui
                .button("Set the loop region to its body")
                .on_hover_text("So the transport loops what the game repeats")
                .clicked()
            {
                wanted.push(Wanted::LoopRegion(*range));
            }
            if ui
                .add_enabled(*range != region, Button::new("Use the loop region"))
                .on_hover_text(game::bars(project, region))
                .clicked()
            {
                let mut a = asset.clone();
                a.kind = GameAssetKind::Loop {
                    range: region,
                    intro_start: intro_start.filter(|i| *i < region.start),
                };
                wanted.push(Wanted::Update(a));
            }
        }
        GameAssetKind::Sfx { variations, .. } => {
            if ui
                .button("Set the loop region to it")
                .on_hover_text("So the transport plays its variations in turn")
                .clicked()
            {
                wanted.push(Wanted::LoopRegion(span(asset)));
            }
            if ui
                .add_enabled(span(asset) != region, Button::new("Use the loop region"))
                .on_hover_text(format!(
                    "{}, split into {} equal parts",
                    game::bars(project, region),
                    variations.len()
                ))
                .clicked()
            {
                let mut a = asset.clone();
                if let GameAssetKind::Sfx { variations, .. } = &mut a.kind {
                    *variations = region.split(variations.len());
                }
                wanted.push(Wanted::Update(a));
            }
        }
    }
    ui.separator();
    if ui.button("Game assets…").clicked() {
        wanted.push(Wanted::Window);
    }
    if ui.button("Remove").clicked() {
        wanted.push(Wanted::Remove(asset.id));
    }
}

/// Paints one region: a loop's intro faint and its body solid with the seam
/// marked, or an SFX set's variations as slices.
fn draw_region(painter: &Painter, st: &ArrangeState, asset: &GameAsset, r: Rect, lit: bool) {
    let c = color(asset);
    let radius = CornerRadius::same(3);
    let x = |t: Ticks| st.x_of(t as f64);
    let mut label_left = r.left();
    match &asset.kind {
        GameAssetKind::Loop { range, intro_start } => {
            let body = Rect::from_min_max(
                pos2(x(range.start), r.top()),
                pos2(x(range.end), r.bottom()),
            );
            if let Some(i) = intro_start {
                let intro = Rect::from_min_max(pos2(x(*i), r.top()), pos2(body.left(), r.bottom()));
                painter.rect_filled(intro, radius, c.gamma_multiply(0.22));
                painter.rect_stroke(
                    intro,
                    radius,
                    Stroke::new(1.0, c.gamma_multiply(0.5)),
                    StrokeKind::Inside,
                );
                if intro.width() > 40.0 {
                    painter.text(
                        pos2(intro.left() + 4.0, intro.center().y),
                        Align2::LEFT_CENTER,
                        "intro",
                        theme::small(),
                        theme::TEXT_WEAK,
                    );
                }
                label_left = body.left();
            }
            painter.rect_filled(body, radius, c.gamma_multiply(0.45));
            // The seam: where the body wraps around to its start.
            painter.vline(body.right() - 1.0, r.y_range(), Stroke::new(2.0, c));
        }
        GameAssetKind::Sfx { variations, .. } => {
            painter.rect_filled(r, radius, c.gamma_multiply(0.15));
            for (i, v) in variations.iter().enumerate() {
                let slice =
                    Rect::from_min_max(pos2(x(v.start), r.top()), pos2(x(v.end), r.bottom()));
                let shade = if i % 2 == 0 { 0.45 } else { 0.32 };
                painter.rect_filled(slice, radius, c.gamma_multiply(shade));
                if i > 0 {
                    painter.vline(slice.left(), r.y_range(), Stroke::new(1.0, theme::BG));
                }
            }
        }
    }
    let stroke = if lit {
        Stroke::new(1.5, theme::lighten(c, 0.6))
    } else {
        Stroke::new(1.0, c.gamma_multiply(0.8))
    };
    painter.rect_stroke(r, radius, stroke, StrokeKind::Inside);
    let text = match &asset.kind {
        GameAssetKind::Loop { .. } => asset.name.clone(),
        GameAssetKind::Sfx { variations, .. } => format!("{} · {}", asset.name, variations.len()),
    };
    if r.right() - label_left > 30.0 {
        let clip = painter.with_clip_rect(r.intersect(painter.clip_rect()));
        clip.text(
            Pos2::new(
                label_left.max(painter.clip_rect().left()) + 5.0,
                r.center().y,
            ),
            Align2::LEFT_CENTER,
            text,
            theme::small(),
            theme::TEXT,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BAR: Ticks = PPQ * 4;

    fn range(start: Ticks, end: Ticks) -> TimeRange {
        TimeRange::new(start, end).unwrap()
    }

    fn asset(kind: GameAssetKind) -> GameAsset {
        GameAsset {
            id: AssetId::new(),
            name: "a".into(),
            category: Default::default(),
            tags: Vec::new(),
            kind,
        }
    }

    fn drag(a: &GameAsset, part: Part, from: Ticks, to: Ticks, grid: Option<Ticks>) -> GameAsset {
        let min_len = grid.unwrap_or(PPQ / 4);
        dragged(a, part, to as f64, from as f64, grid, min_len)
    }

    #[test]
    fn drags_stay_valid() {
        let looped = asset(GameAssetKind::Loop {
            range: range(BAR, 3 * BAR),
            intro_start: Some(0),
        });
        // The body start stops a grid step after the intro and before the end.
        let a = drag(&looped, Part::Start, BAR, -BAR, Some(BAR));
        assert!(matches!(a.kind, GameAssetKind::Loop { range: r, .. } if r.start == BAR));
        let a = drag(&looped, Part::Start, BAR, 9 * BAR, Some(BAR));
        assert!(matches!(a.kind, GameAssetKind::Loop { range: r, .. } if r.start == 2 * BAR));
        // An intro dragged onto the body goes; one dragged before it moves.
        let a = drag(&looped, Part::Intro, 0, 2 * BAR, Some(BAR));
        assert!(matches!(
            a.kind,
            GameAssetKind::Loop {
                intro_start: None,
                ..
            }
        ));
        // Moving never goes before the song.
        let a = drag(&looped, Part::Move, BAR, -5 * BAR, Some(BAR));
        assert_eq!(span(&a), range(0, 3 * BAR));

        // A body shorter than the grid keeps its start rather than passing
        // its intro or its end.
        let short = asset(GameAssetKind::Loop {
            range: range(BAR, BAR + PPQ),
            intro_start: Some(BAR - PPQ),
        });
        assert_eq!(drag(&short, Part::Start, BAR, 0, Some(BAR)), short);

        // A boundary between two touching variations moves for both, and
        // never past either one's far edge.
        let sfx = asset(GameAssetKind::sfx(vec![range(0, PPQ), range(PPQ, 2 * PPQ)]));
        let a = drag(&sfx, Part::Edge(PPQ), PPQ, PPQ / 2, None);
        let GameAssetKind::Sfx { variations, .. } = &a.kind else {
            unreachable!()
        };
        assert_eq!(variations, &[range(0, PPQ / 2), range(PPQ / 2, 2 * PPQ)]);
        let a = drag(&sfx, Part::Edge(PPQ), PPQ, 5 * PPQ, None);
        let GameAssetKind::Sfx { variations, .. } = &a.kind else {
            unreachable!()
        };
        assert_eq!(variations[0].end, 2 * PPQ - PPQ / 4);
        // Variations too short for the grid leave the edge where it is.
        assert_eq!(drag(&sfx, Part::Edge(PPQ), PPQ, 0, Some(BAR)), sfx);
    }
}
