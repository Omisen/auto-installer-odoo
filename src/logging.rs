//! logging setup (gap G1): `tracing` to the TTY **and** to a file.
//!
//! the file captures the whole run, errors and rollback included, as the
//! post-mortem artifact on a customer machine. that layer writes no ANSI
//! sequences, degrades to TTY-only without failing when the path is not
//! writable, and never sees the password — guaranteed by
//! [`crate::secret::Secret`].

use std::fs::{File, OpenOptions};
use std::path::Path;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{fmt, EnvFilter};

/// default log path, owned by root.
///
/// **outside `/opt/odoo`** (A-V3-2, A-R5-2): the log opens before that
/// directory exists, so living there meant the file never appeared on a
/// **first** installation — the one that matters most for a post-mortem.
/// `/var/log` is always there.
///
/// declared consequence: the log **survives the rollback**, deliberately. it is
/// the only account of what happened, and it matters most when something went
/// wrong.
pub const DEFAULT_LOG_PATH: &str = "/var/log/invok.log";

/// opens the log file in append mode, creating it.
///
/// `None` when the path is not writable, which degrades logging to TTY-only
/// rather than failing.
pub fn try_open(path: &Path) -> Option<File> {
    OpenOptions::new().create(true).append(true).open(path).ok()
}

/// initialises `tracing`: a coloured TTY layer plus, when possible, an
/// ANSI-free file layer.
///
/// returns the non-blocking writer's [`WorkerGuard`], which must stay alive for
/// the whole run, or `None` when there is no file. the level comes from
/// `RUST_LOG` and defaults to `info`. in `dry_run` no file is opened, since a
/// preview leaves no artifacts.
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

    // an `Option<Layer>` is itself a `Layer`, a no-op when `None`.
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(tty_layer)
        .with(file_layer)
        .try_init();

    // first line of every run: **who** is writing this log (A-V3-16). here
    // rather than in each entry point, so no future one can forget it.
    tracing::info!(version = crate::INSTALLER_VERSION, "invok");

    guard
}
