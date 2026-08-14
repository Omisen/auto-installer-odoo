//! typed errors for the engine and the steps.
//!
//! every step returns a [`StepError`], promoted to `anyhow::Error` in `main`.
//! each variant must carry enough context for a post-mortem on a customer
//! machine: command name, exit status, path involved.

use std::path::PathBuf;

use thiserror::Error;

/// a domain error produced by a step (snapshot, run or undo).
#[derive(Debug, Error)]
pub enum StepError {
    /// an external command (git, apt, psql, systemctl, …) exited non-zero.
    #[error("comando `{command}` fallito (exit {status}): {stderr}")]
    CommandFailed {
        command: String,
        status: String,
        stderr: String,
    },

    /// an external command outlived its timeout and was killed.
    ///
    /// only **network** operations carry one, against a mirror that never
    /// closes the connection. **retryable**: the clone's retry treats it like
    /// any other failure, then falls back to the tarball. the variable named in
    /// the message is [`crate::system_ops::NETWORK_TIMEOUT_ENV`], and a test
    /// keeps the two in step.
    #[error(
        "comando `{command}` non terminato entro {secs}s: interrotto (timeout di rete). \
         Se la connessione è lenta alza il limite con ODOO_NETWORK_TIMEOUT_SECS=<secondi> \
         (0 = nessun timeout)"
    )]
    Timeout { command: String, secs: u64 },

    /// the snapshot phase could not determine the pre-existing state.
    ///
    /// serious: without a reliable snapshot the undo has no source of truth
    /// (`PreState`) and the rollback is not safe.
    #[error("snapshot fallito per lo step `{step}`: {reason}")]
    SnapshotFailed { step: String, reason: String },

    /// I/O error, carrying the path for diagnosis.
    #[error("errore di I/O su `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// a non-negotiable precondition was violated, such as the hard stop on
    /// initialising a pre-existing database. not recoverable: the installer
    /// stops.
    #[error("precondizione violata: {0}")]
    Precondition(String),

    /// the gevent build failed on an interpreter newer than Odoo's pins
    /// (A-MD-7).
    ///
    /// its own message cannot diagnose it: the reader sees three hundred lines
    /// of `gcc` about `_PyLong_AsByteArray`, and "this Odoo version has no pin
    /// for this Python" appears nowhere. the diagnosis precedes the original
    /// error, which is kept in full — explaining is not hiding the evidence.
    #[error("{diagnosis}\n\n--- errore originale ---\n{original}")]
    PythonTooNew { diagnosis: String, original: String },
}

impl StepError {
    /// builds an [`StepError::Io`] carrying the path involved.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        StepError::Io {
            path: path.into(),
            source,
        }
    }
}
