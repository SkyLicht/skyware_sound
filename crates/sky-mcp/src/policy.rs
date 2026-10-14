// SPDX-License-Identifier: GPL-3.0-or-later
//! What an MCP client may do: a permission level, and the folders file
//! tools may read and write.

use std::path::{Component, Path, PathBuf};

/// How much an MCP client may do. Each level includes the ones before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Permission {
    /// Read the project, analyze and preview.
    ReadOnly,
    /// Also change the project and run the transport.
    Edit,
    /// Also open, save, import and export files inside the allowed folders.
    Files,
}

impl Permission {
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "read" | "readonly" | "read-only" => Some(Self::ReadOnly),
            "edit" => Some(Self::Edit),
            "files" | "full" => Some(Self::Files),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Policy {
    pub permission: Permission,
    /// Folders file tools may use. Relative paths resolve against the first.
    pub roots: Vec<PathBuf>,
}

impl Policy {
    pub fn new(permission: Permission, roots: Vec<PathBuf>) -> Self {
        let roots = roots
            .into_iter()
            .filter_map(|r| {
                let _ = std::fs::create_dir_all(&r);
                std::fs::canonicalize(&r).ok()
            })
            .collect();
        Self { permission, roots }
    }

    /// Resolves a path a client gave and checks it is inside an allowed
    /// folder. The path need not exist yet.
    pub fn resolve(&self, path: &str) -> Result<PathBuf, String> {
        let Some(first) = self.roots.first() else {
            return Err("file access is off: no folders are allowed. \
                 Start Skyware Sound with --allow-dir to allow one."
                .into());
        };
        let given = Path::new(path.trim());
        if given.as_os_str().is_empty() {
            return Err("the path is empty".into());
        }
        if given.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err("paths may not contain \"..\"".into());
        }
        let joined = if given.is_absolute() {
            given.to_path_buf()
        } else {
            first.join(given)
        };
        let resolved = canonicalize_existing_prefix(&joined);
        if self.roots.iter().any(|r| resolved.starts_with(r)) {
            Ok(resolved)
        } else {
            Err(format!(
                "{} is outside the allowed folders: {}",
                joined.display(),
                self.roots
                    .iter()
                    .map(|r| r.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        }
    }
}

/// Canonicalizes the longest part of `path` that exists and appends the
/// rest, so links cannot lead outside an allowed folder.
fn canonicalize_existing_prefix(path: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut cur = path.to_path_buf();
    loop {
        if let Ok(c) = std::fs::canonicalize(&cur) {
            let mut out = c;
            for part in rest.iter().rev() {
                out.push(part);
            }
            return out;
        }
        match (cur.file_name().map(|n| n.to_os_string()), cur.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                cur = parent.to_path_buf();
            }
            _ => return path.to_path_buf(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_stay_inside_the_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("allowed");
        let policy = Policy::new(Permission::Files, vec![root.clone()]);
        let root = std::fs::canonicalize(&root).unwrap();
        assert_eq!(policy.resolve("a/b.wav").unwrap(), root.join("a/b.wav"));
        assert!(policy.resolve(root.join("x.skyproj").to_str().unwrap()).is_ok());
        assert!(policy.resolve("../escape.wav").is_err());
        assert!(policy.resolve(tmp.path().join("other.wav").to_str().unwrap()).is_err());
        let none = Policy::new(Permission::Files, vec![]);
        assert!(none.resolve("a.wav").is_err());
    }

    #[test]
    fn levels_are_ordered() {
        assert!(Permission::Files > Permission::Edit);
        assert!(Permission::Edit > Permission::ReadOnly);
        assert_eq!(Permission::from_name("full"), Some(Permission::Files));
    }
}
