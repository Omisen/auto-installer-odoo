//! Inizializzazione del logging (gap G1): `tracing` verso TTY **e** file.
//!
//! Il file di log cattura l'intera esecuzione (INFO+), errori e rollback
//! inclusi, come artefatto post-mortem su macchina cliente. Il layer file:
//! - **non** usa sequenze ANSI/colori (solo il TTY le usa);
//! - **degrada senza fallire** se il percorso non è scrivibile (es. dry-run
//!   senza root) → resta il solo TTY;
//! - la password non vi finisce mai (garantito dal tipo [`crate::secret::Secret`]).
//!
//! Vive fuori dal trait `Step`.

use std::fs::{File, OpenOptions};
use std::path::Path;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{fmt, EnvFilter};

/// Percorso di default del log file (owned root).
pub const DEFAULT_LOG_PATH: &str = "/opt/odoo/.installer.log";

/// Prova ad aprire (in append, creando) il file di log. `None` se il percorso
/// non è scrivibile: il logging degrada al solo TTY.
pub fn try_open(path: &Path) -> Option<File> {
    OpenOptions::new().create(true).append(true).open(path).ok()
}

/// Inizializza `tracing`: layer TTY (con colori) + eventuale layer file (senza
/// ANSI). Ritorna il [`WorkerGuard`] del writer non-bloccante da tenere in vita
/// per l'intera esecuzione (o `None` se non c'è file). Livello da `RUST_LOG`
/// (default `info`).
///
/// In `dry_run` il file non viene aperto (una preview non lascia artefatti).
pub fn init(dry_run: bool) -> Option<WorkerGuard> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let tty_layer = fmt::layer().with_writer(std::io::stderr);

    let file = if dry_run {
        None
    } else {
        try_open(Path::new(DEFAULT_LOG_PATH))
    };

    let (file_layer, guard) = match file {
        Some(f) => {
            let (writer, guard) = tracing_appender::non_blocking(f);
            let layer = fmt::layer().with_ansi(false).with_writer(writer);
            (Some(layer), Some(guard))
        }
        None => (None, None),
    };

    // `Option<Layer>` è a sua volta un `Layer` (no-op se `None`).
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(tty_layer)
        .with(file_layer)
        .try_init();

    guard
}
