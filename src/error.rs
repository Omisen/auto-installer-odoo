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
    #[error("command `{command}` failed (exit {status}): {stderr}")]
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
        "command `{command}` did not finish within {secs}s: aborted (network timeout). \
         on a slow connection raise the limit with ODOO_NETWORK_TIMEOUT_SECS=<seconds> \
         (0 = no timeout)"
    )]
    Timeout { command: String, secs: u64 },

    /// the snapshot phase could not determine the pre-existing state.
    ///
    /// serious: without a reliable snapshot the undo has no source of truth
    /// (`PreState`) and the rollback is not safe.
    #[error("snapshot failed for step `{step}`: {reason}")]
    SnapshotFailed { step: String, reason: String },

    /// I/O error, carrying the path for diagnosis.
    #[error("I/O error on `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// a non-negotiable precondition was violated, such as the hard stop on
    /// initialising a pre-existing database. not recoverable: the installer
    /// stops.
    #[error("precondition violated: {0}")]
    Precondition(String),

    /// the gevent build failed, whatever the interpreter (A-MD-7, A-V3-28).
    ///
    /// its own message cannot diagnose it: the reader sees three hundred lines
    /// of `gcc` about `_PyLong_AsByteArray`, and "this Odoo version has no pin
    /// for this Python" appears nowhere. the diagnosis precedes the original
    /// error, which is kept in full — explaining is not hiding the evidence.
    ///
    /// it was called `PythonTooNew` until A-V3-28, and the name was the bug in
    /// miniature: it fired **only** above `NEWEST_TESTED_PYTHON`, so Odoo 16 on
    /// Fedora — Python 3.13, exactly the tested one, and a gevent pin that
    /// predates it — got the three hundred lines and nothing else. "too new for
    /// us" and "not covered by *this* Odoo's pins" are different questions.
    #[error("{diagnosis}\n\n--- original error ---\n{original}")]
    GeventBuildFailed { diagnosis: String, original: String },
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
