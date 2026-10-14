// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-user files: the MCP access token and the default projects folder.

use std::fs;
use std::path::PathBuf;

use anyhow::Context;
use directories::{ProjectDirs, UserDirs};

fn dirs() -> anyhow::Result<ProjectDirs> {
    ProjectDirs::from("", "", "Skyware Sound").context("cannot find the user's config folder")
}

/// Where crash reports go.
pub fn data_dir() -> Option<PathBuf> {
    ProjectDirs::from("", "", "Skyware Sound").map(|d| d.data_local_dir().to_path_buf())
}

/// "Documents/Skyware Sound", created if missing.
pub fn documents_dir() -> Option<PathBuf> {
    let dir = UserDirs::new()?.document_dir()?.join("Skyware Sound");
    fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn token_path() -> anyhow::Result<PathBuf> {
    Ok(dirs()?.config_dir().join("mcp-token"))
}

fn new_token() -> String {
    // Two v4 UUIDs: 244 random bits from the operating system.
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

fn write_token(path: &PathBuf, token: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, token)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// The per-install MCP token, made on first use.
pub fn token() -> anyhow::Result<String> {
    let path = token_path()?;
    if let Ok(text) = fs::read_to_string(&path) {
        let t = text.trim();
        if t.len() >= 32 {
            return Ok(t.to_string());
        }
    }
    let token = new_token();
    write_token(&path, &token).with_context(|| format!("cannot write {}", path.display()))?;
    Ok(token)
}

pub fn reset_token() -> anyhow::Result<String> {
    let path = token_path()?;
    let token = new_token();
    write_token(&path, &token).with_context(|| format!("cannot write {}", path.display()))?;
    Ok(token)
}

#[cfg(test)]
mod tests {
    #[test]
    fn tokens_are_long_and_different() {
        let a = super::new_token();
        let b = super::new_token();
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
    }
}
