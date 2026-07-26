//! [`ProgressReporter`]: astrazione del progresso, observer del motore.
//!
//! Il motore ([`crate::engine::Installer`]) notifica gli eventi (start/done/
//! failed per ogni step, e undo durante il rollback) **senza dipendere da
//! `indicatif`**: dipende da questo trait. `indicatif` ne è solo
//! un'implementazione ([`IndicatifReporter`]), selezionata in `main` quando c'è
//! un TTY; altrimenti si usa [`LogReporter`] o [`NoopReporter`].

use indicatif::{ProgressBar, ProgressStyle};
use tracing::{info, warn};

/// Observer del progresso. Metodi con default vuoto: un reporter implementa solo
/// ciò che gli serve.
pub trait ProgressReporter {
    fn step_start(&self, _name: &str, _index: usize, _total: usize) {}
    fn step_done(&self, _name: &str) {}
    fn step_failed(&self, _name: &str) {}
    fn rollback_start(&self, _total: usize) {}
    fn undo_start(&self, _name: &str) {}
    fn undo_done(&self, _name: &str) {}
}

/// Nessun output (no-TTY silenzioso, o quando il progresso non serve).
#[derive(Debug, Default)]
pub struct NoopReporter;
impl ProgressReporter for NoopReporter {}

/// Solo `tracing` — per no-TTY o output su file.
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
        warn!("rollback in corso ({total} step da annullare)…");
    }
    fn undo_start(&self, name: &str) {
        info!("undo: {name}");
    }
}

/// Progress bar / spinner con `indicatif` (TTY interattivo).
pub struct IndicatifReporter {
    bar: ProgressBar,
}

impl IndicatifReporter {
    pub fn new(total: usize) -> Self {
        let bar = ProgressBar::new(total as u64);
        if let Ok(style) =
            ProgressStyle::with_template("{spinner:.green} [{pos}/{len}] {wide_msg}")
        {
            bar.set_style(style);
        }
        bar.enable_steady_tick(std::time::Duration::from_millis(120));
        Self { bar }
    }
}

impl ProgressReporter for IndicatifReporter {
    fn step_start(&self, name: &str, _index: usize, _total: usize) {
        self.bar.set_message(name.to_string());
    }
    fn step_done(&self, _name: &str) {
        self.bar.inc(1);
    }
    fn step_failed(&self, name: &str) {
        self.bar.set_message(format!("fallito: {name}"));
    }
    fn rollback_start(&self, _total: usize) {
        self.bar.set_message("rollback in corso…".to_string());
    }
    fn undo_start(&self, name: &str) {
        self.bar.set_message(format!("undo: {name}"));
    }
}

impl Drop for IndicatifReporter {
    fn drop(&mut self) {
        self.bar.finish_and_clear();
    }
}
