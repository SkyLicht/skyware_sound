// SPDX-License-Identifier: GPL-3.0-or-later
//! Transactions, undo and redo.

use serde::{Deserialize, Serialize};

use crate::command::Command;
use crate::error::ModelError;
use crate::project::Project;

/// Who made a change. MCP changes carry the client and tool, so the AI
/// activity panel can show and undo them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Origin {
    User,
    Mcp { client: String, tool: String },
    Script,
    System,
}

impl Origin {
    pub fn is_mcp(&self) -> bool {
        matches!(self, Origin::Mcp { .. })
    }
}

/// A group of commands that apply, and undo, as one step.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transaction {
    pub label: String,
    pub origin: Origin,
    pub commands: Vec<Command>,
    /// Consecutive transactions with the same key merge into one undo step,
    /// so dragging a fader is one step rather than hundreds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coalesce: Option<u64>,
}

impl Transaction {
    pub fn new(label: impl Into<String>, origin: Origin, commands: Vec<Command>) -> Self {
        Self {
            label: label.into(),
            origin,
            commands,
            coalesce: None,
        }
    }

    pub fn user(label: impl Into<String>, commands: Vec<Command>) -> Self {
        Self::new(label, Origin::User, commands)
    }

    /// A single user command, labelled by what it does.
    pub fn single(command: Command) -> Self {
        Self::user(command.describe(), vec![command])
    }

    pub fn coalescing(mut self, key: u64) -> Self {
        self.coalesce = Some(key);
        self
    }
}

/// A transaction that has been applied.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Applied {
    pub seq: u64,
    pub label: String,
    pub origin: Origin,
    pub forward: Vec<Command>,
    /// Inverses in application order; undo applies them back to front.
    pub inverse: Vec<Command>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coalesce: Option<u64>,
}

/// Undo and redo stacks.
#[derive(Clone, Debug, Default)]
pub struct History {
    undo: Vec<Applied>,
    redo: Vec<Applied>,
    next_seq: u64,
    limit: usize,
}

/// Applies commands in order; if one fails, rolls back the ones already
/// applied so the project is left exactly as it was.
pub fn apply_all(
    project: &mut Project,
    commands: Vec<Command>,
) -> Result<(Vec<Command>, Vec<Command>), ModelError> {
    let mut forward = Vec::with_capacity(commands.len());
    let mut inverse = Vec::with_capacity(commands.len());
    for (i, cmd) in commands.into_iter().enumerate() {
        match cmd.clone().apply(project) {
            Ok(inv) => {
                forward.push(cmd);
                inverse.push(inv);
            }
            Err(e) => {
                for inv in inverse.into_iter().rev() {
                    // Inverses of commands that just succeeded cannot fail.
                    let _ = inv.apply(project);
                }
                return Err(if i == 0 {
                    e
                } else {
                    ModelError::InvalidValue(format!("step {} of the change failed: {e}", i + 1))
                });
            }
        }
    }
    Ok((forward, inverse))
}

fn apply_inverse(project: &mut Project, inverse: &[Command]) -> Result<(), ModelError> {
    apply_all(project, inverse.iter().rev().cloned().collect()).map(|_| ())
}

impl History {
    pub fn new() -> Self {
        Self {
            limit: 1_000,
            ..Self::default()
        }
    }

    /// Applies a transaction as one undo step.
    pub fn apply(
        &mut self,
        project: &mut Project,
        tx: Transaction,
    ) -> Result<&Applied, ModelError> {
        if tx.commands.is_empty() {
            return Err(ModelError::NothingTo("apply"));
        }
        let (forward, inverse) = apply_all(project, tx.commands)?;
        self.redo.clear();
        let merge = tx.coalesce.is_some()
            && self
                .undo
                .last()
                .is_some_and(|last| last.coalesce == tx.coalesce && last.origin == tx.origin);
        if merge && let Some(last) = self.undo.last_mut() {
            last.forward.extend(forward);
            last.inverse.extend(inverse);
        } else {
            self.next_seq += 1;
            self.undo.push(Applied {
                seq: self.next_seq,
                label: tx.label,
                origin: tx.origin,
                forward,
                inverse,
                coalesce: tx.coalesce,
            });
            if self.limit > 0 && self.undo.len() > self.limit {
                self.undo.remove(0);
            }
        }
        // The stack was just pushed or extended.
        self.undo.last().ok_or(ModelError::NothingTo("apply"))
    }

    /// Undoes the latest step and returns it.
    pub fn undo(&mut self, project: &mut Project) -> Result<Applied, ModelError> {
        let step = self.undo.pop().ok_or(ModelError::NothingTo("undo"))?;
        if let Err(e) = apply_inverse(project, &step.inverse) {
            self.undo.push(step);
            return Err(e);
        }
        self.redo.push(step.clone());
        Ok(step)
    }

    /// Redoes the latest undone step and returns it.
    pub fn redo(&mut self, project: &mut Project) -> Result<Applied, ModelError> {
        let mut step = self.redo.pop().ok_or(ModelError::NothingTo("redo"))?;
        match apply_all(project, step.forward.clone()) {
            Ok((_, inverse)) => {
                step.inverse = inverse;
                self.undo.push(step.clone());
                Ok(step)
            }
            Err(e) => {
                self.redo.push(step);
                Err(e)
            }
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|a| a.label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|a| a.label.as_str())
    }

    /// The undo stack, oldest first.
    pub fn undo_steps(&self) -> &[Applied] {
        &self.undo
    }

    pub fn redo_steps(&self) -> &[Applied] {
        &self.redo
    }

    /// Sequence number of the step undo would revert next.
    pub fn top_seq(&self) -> Option<u64> {
        self.undo.last().map(|a| a.seq)
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }
}
