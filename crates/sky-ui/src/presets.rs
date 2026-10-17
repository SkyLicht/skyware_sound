// SPDX-License-Identifier: GPL-3.0-or-later
//! Presets and A/B comparison for instruments and effects: the bar at the
//! top of an effect window and of the instrument panel.
//!
//! Factory presets come with the app; the user's own are kept in one JSON
//! file. Loading a preset or switching between A and B replaces all of a
//! device's values in one undo step. The preset shown is whichever one the
//! values match, so undo, AI edits and A/B all keep it right.

use std::collections::BTreeMap;
use std::path::PathBuf;

use egui::{ComboBox, Key, RichText, TextEdit, Ui};
use serde::{Deserialize, Serialize};
use sky_core::{EffectId, TrackId};
use sky_dsp::{EffectType, InstrumentType, ParamKind, ParamSpec, Preset};
use sky_model::{AbCompare, ChannelRef};

use crate::actions;
use crate::app::App;
use crate::theme;

/// The longest preset name.
const MAX_NAME: usize = 64;

/// A device in the project whose settings the bar changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Device {
    Effect(ChannelRef, EffectId),
    Instrument(TrackId),
}

/// What kind of device it is, which decides its presets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Effect(EffectType),
    Instrument(InstrumentType),
}

impl Kind {
    pub fn specs(self) -> &'static [ParamSpec] {
        match self {
            Kind::Effect(t) => t.params(),
            Kind::Instrument(t) => t.params(),
        }
    }

    pub fn factory(self) -> &'static [Preset] {
        match self {
            Kind::Effect(t) => t.presets(),
            Kind::Instrument(t) => t.presets(),
        }
    }

    /// How the preset file names the kind, such as `effect/eq`.
    fn key(self) -> String {
        match self {
            Kind::Effect(t) => format!("effect/{}", t.name()),
            Kind::Instrument(t) => format!("instrument/{}", t.name()),
        }
    }
}

/// A preset the user saved.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserPreset {
    /// The kind of device it is for, such as `effect/eq`.
    pub device: String,
    pub name: String,
    /// Values that differ from the defaults.
    #[serde(default)]
    pub values: BTreeMap<String, f32>,
}

#[derive(Default, Serialize, Deserialize)]
struct PresetFile {
    #[serde(default)]
    presets: Vec<UserPreset>,
}

/// The user's presets and the file that keeps them.
#[derive(Default)]
pub struct PresetStore {
    path: Option<PathBuf>,
    presets: Vec<UserPreset>,
    /// The file was there but could not be read, so the first save keeps
    /// a copy of it before writing a new one.
    unreadable: bool,
}

impl PresetStore {
    /// Reads the user's presets from `path`; `None` keeps them only while
    /// the app runs.
    pub fn load(path: Option<PathBuf>) -> Self {
        let mut store = Self {
            path,
            ..Self::default()
        };
        let Some(path) = &store.path else {
            return store;
        };
        match std::fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str::<PresetFile>(&text) {
                Ok(file) => store.presets = file.presets,
                Err(e) => {
                    tracing::warn!("cannot read the presets in {}: {e}", path.display());
                    store.unreadable = true;
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!("cannot read the presets in {}: {e}", path.display());
                store.unreadable = true;
            }
        }
        store
    }

    /// The user's presets for one kind of device, by name.
    pub fn of(&self, kind: Kind) -> impl Iterator<Item = &UserPreset> {
        let key = kind.key();
        self.presets.iter().filter(move |p| p.device == key)
    }

    /// Saves a preset, replacing one of the same name.
    pub fn save(
        &mut self,
        kind: Kind,
        name: &str,
        values: BTreeMap<String, f32>,
    ) -> Result<(), String> {
        let key = kind.key();
        match self
            .presets
            .iter_mut()
            .find(|p| p.device == key && p.name == name)
        {
            Some(p) => p.values = values,
            None => self.presets.push(UserPreset {
                device: key,
                name: name.to_string(),
                values,
            }),
        }
        self.presets.sort_by(|a, b| {
            (&a.device, a.name.to_lowercase()).cmp(&(&b.device, b.name.to_lowercase()))
        });
        self.write()
    }

    pub fn delete(&mut self, kind: Kind, name: &str) -> Result<(), String> {
        let key = kind.key();
        self.presets
            .retain(|p| !(p.device == key && p.name == name));
        self.write()
    }

    fn write(&mut self) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let failed = |e: std::io::Error| {
            format!(
                "The preset is kept until the app closes: cannot write {}: {e}",
                path.display()
            )
        };
        if self.unreadable {
            let kept = path.with_extension("json.unreadable");
            std::fs::rename(path, &kept).map_err(failed)?;
            tracing::warn!("kept the unreadable presets file as {}", kept.display());
            self.unreadable = false;
        }
        let file = PresetFile {
            presets: self.presets.clone(),
        };
        let text = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(failed)?;
        }
        // Write beside it and swap, so a crash never leaves half a file.
        let next = path.with_extension("json.new");
        std::fs::write(&next, text).map_err(failed)?;
        std::fs::rename(&next, path).map_err(failed)
    }
}

#[derive(Default)]
pub struct PresetState {
    pub store: PresetStore,
    /// The preset being named, while Save is open.
    pub naming: Option<Naming>,
}

/// A preset being named before it is saved.
pub struct Naming {
    /// The device it is saved from.
    pub device: Device,
    pub name: String,
    /// The name field still has to take the keyboard.
    focus: bool,
}

/// Whether two values of a parameter are the same setting.
fn same(spec: &ParamSpec, a: f32, b: f32) -> bool {
    (a - b).abs() <= (spec.max - spec.min) * 1e-5
}

fn value_in(spec: &ParamSpec, params: &BTreeMap<String, f32>) -> f32 {
    params.get(spec.name).copied().unwrap_or(spec.default)
}

/// The values to store for a device: those of `values` that are known,
/// kept in range and differ from the defaults.
pub fn tidy(specs: &[ParamSpec], values: impl Fn(&str) -> Option<f32>) -> BTreeMap<String, f32> {
    let mut out = BTreeMap::new();
    for spec in specs {
        let Some(v) = values(spec.name).filter(|v| v.is_finite()) else {
            continue;
        };
        let mut v = v.clamp(spec.min, spec.max);
        if !matches!(spec.kind, ParamKind::Continuous { .. }) {
            v = v.round();
        }
        if !same(spec, v, spec.default) {
            out.insert(spec.name.to_string(), v);
        }
    }
    out
}

/// Whether two sets of values sound the same.
fn alike(specs: &[ParamSpec], a: &BTreeMap<String, f32>, b: &BTreeMap<String, f32>) -> bool {
    specs
        .iter()
        .all(|s| same(s, value_in(s, a), value_in(s, b)))
}

/// The preset a device's values match.
#[derive(Clone, Debug, PartialEq)]
pub enum Current {
    Default,
    Factory(&'static str),
    User(String),
    /// Values no preset has.
    Custom,
}

impl Current {
    fn name(&self) -> &str {
        match self {
            Current::Default => "Default",
            Current::Factory(n) => n,
            Current::User(n) => n,
            Current::Custom => "Custom",
        }
    }
}

/// The preset `params` match. The user's own names come first, so a
/// preset saved from a factory one shows the name it was saved as.
pub fn current(store: &PresetStore, kind: Kind, params: &BTreeMap<String, f32>) -> Current {
    let specs = kind.specs();
    if let Some(p) = store.of(kind).find(|p| alike(specs, params, &p.values)) {
        return Current::User(p.name.clone());
    }
    if alike(specs, params, &BTreeMap::new()) {
        return Current::Default;
    }
    kind.factory()
        .iter()
        .find(|p| alike(specs, params, &tidy(specs, |n| p.value(n))))
        .map_or(Current::Custom, |p| Current::Factory(p.name))
}

/// Replaces a device's values and its A/B comparison in one undo step.
fn apply(
    app: &mut App,
    device: Device,
    params: BTreeMap<String, f32>,
    compare: Option<AbCompare>,
    label: &str,
) {
    let r = match device {
        Device::Effect(channel, effect) => {
            actions::set_effect_settings(&app.session, channel, effect, params, compare, label)
        }
        Device::Instrument(track) => {
            actions::set_instrument_settings(&app.session, track, params, compare, label)
        }
    };
    app.report(r);
}

/// A name a preset can have, or why not.
fn check_name(name: &str) -> Result<&str, &'static str> {
    let name = name.trim();
    if name.is_empty() {
        Err("Type a name for the preset")
    } else if name.chars().count() > MAX_NAME {
        Err("Preset names are at most 64 characters")
    } else if name.chars().any(char::is_control) {
        Err("Preset names are one line of text")
    } else {
        Ok(name)
    }
}

/// The preset menu, Save and Delete, and the A and B buttons, for one
/// device whose values are `params`.
pub fn bar(
    app: &mut App,
    ui: &mut Ui,
    device: Device,
    kind: Kind,
    params: &BTreeMap<String, f32>,
    compare: Option<&AbCompare>,
) {
    let specs = kind.specs();
    let now = current(&app.presets.store, kind, params);
    ui.horizontal(|ui| {
        ui.label("Preset");
        let mut picked: Option<(String, BTreeMap<String, f32>)> = None;
        let mine: Vec<UserPreset> = app.presets.store.of(kind).cloned().collect();
        ComboBox::from_id_salt(("preset", device))
            .selected_text(match now {
                Current::Custom => RichText::new("Custom").color(theme::TEXT_WEAK),
                _ => RichText::new(now.name()),
            })
            .width(150.0)
            .height(480.0)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(now == Current::Default, "Default")
                    .on_hover_text("Every parameter at its default")
                    .clicked()
                {
                    picked = Some(("Default".into(), BTreeMap::new()));
                }
                for p in kind.factory() {
                    if ui
                        .selectable_label(now == Current::Factory(p.name), p.name)
                        .clicked()
                    {
                        picked = Some((p.name.into(), tidy(specs, |n| p.value(n))));
                    }
                }
                if !mine.is_empty() {
                    ui.separator();
                    ui.label(RichText::new("Your presets").small().color(theme::TEXT_DIM));
                }
                for p in &mine {
                    if ui
                        .selectable_label(now == Current::User(p.name.clone()), &p.name)
                        .clicked()
                    {
                        picked = Some((p.name.clone(), tidy(specs, |n| p.values.get(n).copied())));
                    }
                }
            })
            .response
            .on_hover_text("Load a preset: it replaces every value, in one undo step");
        if let Some((name, values)) = picked
            && values != *params
        {
            apply(
                app,
                device,
                values,
                compare.cloned(),
                &format!("Load preset “{name}”"),
            );
        }

        if let Some(naming) = &mut app.presets.naming
            && naming.device == device
        {
            let field = ui.add(
                TextEdit::singleline(&mut naming.name)
                    .hint_text("Preset name")
                    .desired_width(130.0),
            );
            if std::mem::take(&mut naming.focus) {
                field.request_focus();
            }
            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            let escape = ui.input(|i| i.key_pressed(Key::Escape));
            let typed = naming.name.clone();
            let valid = check_name(&typed);
            let taken = valid.is_ok_and(|n| mine.iter().any(|p| p.name == n));
            let save = ui
                .add_enabled(valid.is_ok(), egui::Button::new("Save"))
                .on_hover_text(if taken {
                    "Replace your preset of this name"
                } else {
                    "Save these values as a preset"
                })
                .on_disabled_hover_text(valid.err().unwrap_or_default())
                .clicked();
            let cancel = ui.button("Cancel").clicked();
            if let Ok(name) = valid
                && (save || enter)
            {
                let r = app.presets.store.save(kind, name, params.clone());
                if app.report(r).is_some() {
                    app.info(format!("Saved preset “{name}”"));
                }
                app.presets.naming = None;
            } else if cancel || escape {
                app.presets.naming = None;
            }
        } else {
            if ui
                .button("Save…")
                .on_hover_text("Save these values as one of your presets")
                .clicked()
            {
                let suggested = match &now {
                    Current::User(n) => n.clone(),
                    _ => String::new(),
                };
                app.presets.naming = Some(Naming {
                    device,
                    name: suggested,
                    focus: true,
                });
            }
            if let Current::User(name) = &now
                && ui
                    .button("Delete")
                    .on_hover_text(format!(
                        "Delete your preset “{name}”. The values stay on this device."
                    ))
                    .clicked()
            {
                let r = app.presets.store.delete(kind, name);
                if app.report(r).is_some() {
                    app.info(format!("Deleted preset “{name}”"));
                }
            }
        }

        ui.separator();
        ab_buttons(app, ui, device, specs, params, compare);
    });
}

/// A and B: two versions of the values to switch between while listening.
fn ab_buttons(
    app: &mut App,
    ui: &mut Ui,
    device: Device,
    specs: &[ParamSpec],
    params: &BTreeMap<String, f32>,
    compare: Option<&AbCompare>,
) {
    // Before a comparison starts, B is a copy of A.
    let started = compare.cloned().unwrap_or_else(|| AbCompare {
        on_b: false,
        other: params.clone(),
    });
    let on_b = started.on_b;
    let hint = "Keep two versions of the values and switch between them while listening";
    for (b, label) in [(false, "A"), (true, "B")] {
        if ui
            .selectable_label(on_b == b, label)
            .on_hover_text(hint)
            .clicked()
            && on_b != b
        {
            let next = started.switched(params);
            apply(
                app,
                device,
                started.other.clone(),
                Some(next),
                &format!("Switch to {label}"),
            );
        }
    }
    let (from, to) = if on_b { ("B", "A") } else { ("A", "B") };
    if ui
        .add_enabled(
            !alike(specs, params, &started.other),
            egui::Button::new(format!("Copy {from} to {to}")),
        )
        .on_hover_text(format!("Make {to} the same as {from}"))
        .clicked()
    {
        let copied = AbCompare {
            on_b,
            other: params.clone(),
        };
        apply(
            app,
            device,
            params.clone(),
            Some(copied),
            &format!("Copy {from} to {to}"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_tidied_and_matched_to_presets() {
        let kind = Kind::Effect(EffectType::Filter);
        let specs = kind.specs();
        let tidied = tidy(specs, |n| match n {
            "mode" => Some(1.2),
            "cutoff" => Some(1e9),
            "resonance" => Some(0.2),
            _ => Some(f32::NAN),
        });
        assert_eq!(
            tidied,
            [("mode".to_string(), 1.0), ("cutoff".to_string(), 20_000.0)].into(),
            "rounded, clamped, defaults and unknowns left out"
        );
        let store = PresetStore::default();
        assert_eq!(current(&store, kind, &BTreeMap::new()), Current::Default);
        let rumble: BTreeMap<String, f32> = [
            ("mode".to_string(), 1.0),
            ("cutoff".to_string(), 35.000_1),
            ("resonance".to_string(), 0.1),
        ]
        .into();
        assert_eq!(
            current(&store, kind, &rumble),
            Current::Factory("Rumble cut")
        );
        assert_eq!(current(&store, kind, &tidied), Current::Custom);
    }

    #[test]
    fn the_store_keeps_presets_in_a_file() {
        let dir = std::env::temp_dir().join(format!("sky-presets-{}", std::process::id()));
        let path = dir.join("presets.json");
        let _ = std::fs::remove_dir_all(&dir);
        let kind = Kind::Instrument(InstrumentType::Acid);
        let values: BTreeMap<String, f32> = [("cutoff".to_string(), 220.0)].into();
        let mut store = PresetStore::load(Some(path.clone()));
        store.save(kind, "Mine", values.clone()).unwrap();
        store.save(kind, "another", BTreeMap::new()).unwrap();
        store
            .save(Kind::Effect(EffectType::Eq), "Mine", BTreeMap::new())
            .unwrap();
        let back = PresetStore::load(Some(path.clone()));
        let names: Vec<&str> = back.of(kind).map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["another", "Mine"]);
        assert_eq!(current(&back, kind, &values), Current::User("Mine".into()));
        let mut back = back;
        back.delete(kind, "Mine").unwrap();
        let again = PresetStore::load(Some(path.clone()));
        assert_eq!(again.of(kind).count(), 1);
        assert_eq!(again.of(Kind::Effect(EffectType::Eq)).count(), 1);

        // A broken file is kept aside, not overwritten.
        std::fs::write(&path, "{ not json").unwrap();
        let mut broken = PresetStore::load(Some(path.clone()));
        assert_eq!(broken.of(kind).count(), 0);
        broken.save(kind, "New", values).unwrap();
        assert_eq!(
            std::fs::read_to_string(path.with_extension("json.unreadable")).unwrap(),
            "{ not json"
        );
        assert_eq!(PresetStore::load(Some(path)).of(kind).count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_are_one_short_line() {
        assert_eq!(check_name("  Warm  "), Ok("Warm"));
        assert!(check_name(" ").is_err());
        assert!(check_name("a\nb").is_err());
        assert!(check_name(&"x".repeat(65)).is_err());
    }
}
