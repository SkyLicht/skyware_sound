// SPDX-License-Identifier: GPL-3.0-or-later
//! Work that must not block a frame: renders, exports, file I/O, native file
//! dialogs and waveform peaks. Each job runs on its own thread and sends its
//! result back over a channel that the UI polls once per frame.

use std::sync::mpsc::{Receiver, Sender, channel};

use crate::app::Action;
use crate::dialogs::{LoopSummary, PathPurpose, WavSummary};
use crate::waveform::Peaks;
use sky_core::AudioId;
use sky_library::Pack;
use sky_session::BankLoaded;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// What a finished job hands back.
pub enum JobOutput {
    Saved {
        result: Result<PathBuf, String>,
        /// An action waiting for the save, such as opening another project.
        then: Option<Action>,
    },
    Opened(Result<(PathBuf, usize), String>),
    Imported {
        name: String,
        result: Result<AudioId, String>,
    },
    LoopExported(Result<LoopSummary, String>),
    WavExported(Result<WavSummary, String>),
    PathPicked {
        purpose: PathPurpose,
        token: u64,
        path: Option<PathBuf>,
        elapsed: Duration,
    },
    Devices(Vec<String>),
    Peaks(AudioId, Option<Arc<Peaks>>),
    /// A sound pack's sample map, read for the Load sounds dialog.
    PackListed {
        map: String,
        result: Result<Arc<Pack>, String>,
    },
    BankLoaded(Result<BankLoaded, String>),
}

struct Running {
    id: u64,
    label: Option<String>,
}

/// Background jobs and their results.
pub struct Jobs {
    tx: Sender<(u64, JobOutput)>,
    rx: Receiver<(u64, JobOutput)>,
    running: Vec<Running>,
    next_id: u64,
}

impl Default for Jobs {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            tx,
            rx,
            running: Vec::new(),
            next_id: 1,
        }
    }
}

impl Jobs {
    /// Runs `work` on a new thread. Jobs with a label show a spinner in the
    /// status bar until they finish.
    pub fn spawn(
        &mut self,
        ctx: &egui::Context,
        label: Option<String>,
        work: impl FnOnce() -> JobOutput + Send + 'static,
    ) {
        let id = self.next_id;
        self.next_id += 1;
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("sky-ui-job".into())
            .spawn(move || {
                let out = work();
                // The receiver only goes away when the app closes.
                let _ = tx.send((id, out));
                ctx.request_repaint();
            });
        match spawned {
            Ok(_) => self.running.push(Running { id, label }),
            Err(e) => tracing::error!("cannot start a background job: {e}"),
        }
    }

    /// Results that arrived since the last call.
    pub fn poll(&mut self) -> Vec<JobOutput> {
        let mut out = Vec::new();
        while let Ok((id, result)) = self.rx.try_recv() {
            self.running.retain(|r| r.id != id);
            out.push(result);
        }
        out
    }

    /// Labels of the jobs still running that the user should know about.
    pub fn busy_labels(&self) -> impl Iterator<Item = &str> {
        self.running.iter().filter_map(|r| r.label.as_deref())
    }

    pub fn is_busy(&self) -> bool {
        self.running.iter().any(|r| r.label.is_some())
    }

    /// Waits for every running job and returns their results. For tests.
    #[cfg(test)]
    pub fn wait_all(&mut self, timeout: Duration) -> Vec<JobOutput> {
        let deadline = std::time::Instant::now() + timeout;
        let mut out = Vec::new();
        while !self.running.is_empty() {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                break;
            }
            if let Ok((id, result)) = self.rx.recv_timeout(left) {
                self.running.retain(|r| r.id != id);
                out.push(result);
            }
        }
        out
    }
}
