// SPDX-License-Identifier: GPL-3.0-or-later
//! Lists the instrument files built into the program: everything under
//! `instruments/`, as `(path, include_bytes!(...))` pairs.

use std::fmt::Write;
use std::path::{Path, PathBuf};

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("instruments");
    println!("cargo:rerun-if-changed=instruments");
    let mut files = Vec::new();
    walk(&root, &mut files);
    files.sort();
    let mut list = String::from("&[\n");
    for path in &files {
        let rel = path
            .strip_prefix(&root)
            .expect("inside instruments")
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let abs = path.to_string_lossy();
        writeln!(list, "    ({rel:?}, include_bytes!({abs:?})),").expect("write");
    }
    list.push(']');
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("bundled.rs");
    std::fs::write(out, list).expect("write bundled.rs");
}
