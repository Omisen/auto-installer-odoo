//! [`ProgressReporter`]: the engine's progress observer.
//!
//! [`crate::engine::Installer`] reports step and undo events **without
//! depending on `indicatif`**: it depends on this trait. `indicatif` is just
//! one implementation, picked by `main` when there is a TTY; otherwise
//! [`LogReporter`] or [`NoopReporter`].

use std::sync::Once;

use indicatif::{ProgressBar, ProgressStyle};
use tracing::{info, warn};

/// progress observer. every method defaults to a no-op, so a reporter
/// implements only what it needs.
pub trait ProgressReporter {
    fn step_start(&self, _name: &str, _index: usize, _total: usize) {}
    fn step_done(&self, _name: &str) {}
    fn step_failed(&self, _name: &str) {}
    fn rollback_start(&self, _total: usize) {}
    fn undo_start(&self, _name: &str) {}
    fn undo_done(&self, _name: &str) {}
}

/// no output at all.
#[derive(Debug, Default)]
pub struct NoopReporter;
impl ProgressReporter for NoopReporter {}

/// `tracing` only, for no-TTY runs or output redirected to a file.
#[derive(Debug, Default)]
pub struct LogReporter;

impl ProgressReporter for LogReporter {
    fn step_start(&self, name: &str, index: usize, total: usize) {
        info!("[{}/{}] {}…", index + 1, total, name);
    }
    fn step_done(&self, name: &str) {
        info!("✔ {name}");
    }
    fn step_failed(&self, name: &str) {
        warn!("✖ {name}");
    }
    fn rollback_start(&self, total: usize) {
        warn!("rollback in progress ({total} steps to undo)…");
    }
    fn undo_start(&self, name: &str) {
        info!("undo: {name}");
    }
}

/// progress bar and spinner, for an interactive TTY.
pub struct IndicatifReporter {
    bar: ProgressBar,
    ticking: Once,
}

impl IndicatifReporter {
    pub fn new(total: usize) -> Self {
        let bar = ProgressBar::new(total as u64);
        if let Ok(style) = ProgressStyle::with_template("{spinner:.green} [{pos}/{len}] {wide_msg}")
        {
            bar.set_style(style);
        }
        Self {
            bar,
            ticking: Once::new(),
        }
    }

    /// starts the ticker on the **first event**, not at construction.
    ///
    /// `enable_steady_tick` redraws the bar on stderr, the same stream
    /// `inquire` writes to: a live bar during a prompt erases the user's line.
    /// the terminal is claimed by the first step, when the questions are over
    /// by construction.
    fn ensure_ticking(&self) {
        self.ticking.call_once(|| {
            self.bar
                .enable_steady_tick(std::time::Duration::from_millis(120));
        });
    }
}

impl ProgressReporter for IndicatifReporter {
    fn step_start(&self, name: &str, _index: usize, _total: usize) {
        self.ensure_ticking();
        self.bar.set_message(name.to_string());
    }
    fn step_done(&self, _name: &str) {
        self.bar.inc(1);
    }
    fn step_failed(&self, name: &str) {
        self.bar.set_message(format!("failed: {name}"));
    }
    fn rollback_start(&self, _total: usize) {
        self.ensure_ticking();
        self.bar.set_message("rollback in progress…".to_string());
    }
    fn undo_start(&self, name: &str) {
        self.ensure_ticking();
        self.bar.set_message(format!("undo: {name}"));
    }
}

impl Drop for IndicatifReporter {
    fn drop(&mut self) {
        self.bar.finish_and_clear();
    }
}
