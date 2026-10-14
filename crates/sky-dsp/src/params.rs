// SPDX-License-Identifier: GPL-3.0-or-later
//! Parameter descriptions.
//!
//! Each instrument and effect publishes its parameters as a static list of
//! [`ParamSpec`]s. The project model validates values against them, the UI
//! draws controls from them and the MCP server lists them to AI clients.

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum ParamKind {
    /// A continuous value. `log` means the control should move on a log scale.
    Continuous { log: bool },
    /// One of a fixed list; the value is the option's index.
    Choice { options: &'static [&'static str] },
    /// Off (0) or on (1).
    Toggle,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParamSpec {
    /// The stable name used in project files and MCP calls.
    pub name: &'static str,
    pub label: &'static str,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub unit: &'static str,
    pub kind: ParamKind,
}

impl ParamSpec {
    pub const fn linear(
        name: &'static str,
        label: &'static str,
        min: f32,
        max: f32,
        default: f32,
        unit: &'static str,
    ) -> Self {
        Self {
            name,
            label,
            min,
            max,
            default,
            unit,
            kind: ParamKind::Continuous { log: false },
        }
    }

    pub const fn log(
        name: &'static str,
        label: &'static str,
        min: f32,
        max: f32,
        default: f32,
        unit: &'static str,
    ) -> Self {
        Self {
            name,
            label,
            min,
            max,
            default,
            unit,
            kind: ParamKind::Continuous { log: true },
        }
    }

    pub const fn choice(
        name: &'static str,
        label: &'static str,
        options: &'static [&'static str],
        default: f32,
    ) -> Self {
        Self {
            name,
            label,
            min: 0.0,
            max: (options.len() - 1) as f32,
            default,
            unit: "",
            kind: ParamKind::Choice { options },
        }
    }

    pub const fn toggle(name: &'static str, label: &'static str, default: bool) -> Self {
        Self {
            name,
            label,
            min: 0.0,
            max: 1.0,
            default: if default { 1.0 } else { 0.0 },
            unit: "",
            kind: ParamKind::Toggle,
        }
    }

    /// Checks a value against the range, returning it rounded for choices and toggles.
    pub fn validate(&self, value: f32) -> Result<f32, String> {
        if !value.is_finite() {
            return Err(format!("{} must be a finite number", self.name));
        }
        if value < self.min || value > self.max {
            return Err(format!(
                "{} must be between {} and {}{}, got {}",
                self.name,
                self.min,
                self.max,
                if self.unit.is_empty() {
                    String::new()
                } else {
                    format!(" {}", self.unit)
                },
                value
            ));
        }
        Ok(match self.kind {
            ParamKind::Continuous { .. } => value,
            _ => value.round(),
        })
    }

    /// For choice parameters, the index of an option given by name.
    pub fn option_index(&self, option: &str) -> Option<f32> {
        match self.kind {
            ParamKind::Choice { options } => options
                .iter()
                .position(|o| o.eq_ignore_ascii_case(option))
                .map(|i| i as f32),
            _ => None,
        }
    }
}

/// Finds a spec by name.
pub fn find<'a>(specs: &'a [ParamSpec], name: &str) -> Option<&'a ParamSpec> {
    specs.iter().find(|s| s.name == name)
}
