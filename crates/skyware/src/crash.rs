// SPDX-License-Identifier: GPL-3.0-or-later
//! Panics are logged and written to a crash report with a backtrace.

use std::backtrace::Backtrace;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = Backtrace::force_capture();
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("unnamed");
        let report = format!(
            "Skyware Sound {} crashed on thread '{name}'.\n{info}\n\nBacktrace:\n{backtrace}\n",
            env!("CARGO_PKG_VERSION")
        );
        tracing::error!("{info}");
        if let Some(dir) = crate::config::data_dir() {
            let secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let path = dir.join("crashes").join(format!("crash-{secs}.txt"));
            let written = std::fs::create_dir_all(dir.join("crashes"))
                .and_then(|()| std::fs::write(&path, &report));
            if written.is_ok() {
                eprintln!("A crash report was written to {}", path.display());
            }
        }
        default(info);
    }));
}
