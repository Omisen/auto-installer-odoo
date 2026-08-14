//! Ctrl-C and `kill` handled rather than suffered (B-V3-5).
//!
//! # why the handler is tiny
//!
//! the signal reaches the **whole process group**, so `apt`, `git` and `pip`
//! die anyway; the child command comes back failed and the engine already
//! rolls back for that case. we only need to *survive*, so the handler raises
//! a flag and nothing else. before R18 the default action killed us outright
//! and the in-process rollback never ran.
//!
//! no `unsafe`: `signal_hook` wraps the async-signal-safe part behind a safe
//! API, and it was already in the tree as a transitive dependency of
//! `crossterm`. zero new dependencies, no `unsafe` in a program running as
//! root.
//!
//! # the second Ctrl-C really exits
//!
//! `register_conditional_shutdown` exits with 130 **when the flag is already
//! raised**. registered first, it sees the flag still false on the first
//! signal and lets the run proceed — so the registration order is part of the
//! behaviour, not a detail. the system is then genuinely left half-done, by
//! the user's choice, and `invok rollback` cleans it up.
//!
//! # sending the signal from a script
//!
//! "two Ctrl-C" means **two signals received**, not two keypresses. from a
//! terminal the distinction never shows: Ctrl-C reaches the process group
//! once, and `sudo` without a pty does not forward it. but
//! `sudo pkill -INT -f invok` hits **two** processes — the `sudo` and the
//! installer — so the installer sees two signals in quick succession and takes
//! the emergency exit instead of rolling back. use the exact name:
//!
//! ```text
//! sudo pkill -INT -x invok
//! ```
//!
//! not hypothetical: the CI job that exercises this feature failed its first
//! run for exactly that reason.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use tracing::warn;

use crate::error::StepError;

/// conventional exit code for "terminated by SIGINT" (128 + 2).
const EXIT_SIGINT: i32 = 130;

/// registers the handlers and returns the flag the engine watches.
///
/// covers `SIGINT` and `SIGTERM`: two ways of asking the same thing, and
/// handling only one would leave the unattended case — a machine shutting down
/// mid-installation — uncovered.
///
/// a registration failure is **not** fatal: the run proceeds without handling,
/// exactly as before. refusing to install because a handler could not be
/// registered would be disproportionate.
pub fn install() -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));

    for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        // order matters: the conditional exit first (it acts only once the flag
        // is already raised), then the one that raises it. inverted, the first
        // Ctrl-C would kill the process.
        if let Err(e) =
            signal_hook::flag::register_conditional_shutdown(signal, EXIT_SIGINT, Arc::clone(&flag))
        {
            warn!(signal, error = %e, "impossibile registrare l'uscita al secondo segnale");
        }
        if let Err(e) = signal_hook::flag::register(signal, Arc::clone(&flag)) {
            warn!(
                signal,
                error = %e,
                "impossibile gestire questo segnale: un'interruzione ucciderà il processo \
                 senza annullare nulla (usa `invok rollback` per ripulire)"
            );
        }
    }

    flag
}

/// the error used when a run stops on an interruption.
///
/// a `StepError::Precondition` rather than its own variant: this is not a
/// step's defect but a decision taken from outside. the message says what
/// happened **and what was done about it**, because that is what whoever just
/// pressed Ctrl-C wants to know.
pub fn interrupted_error() -> StepError {
    StepError::Precondition(
        "installazione interrotta su richiesta (Ctrl-C o segnale di terminazione).\n\
         Gli step già eseguiti vengono annullati: al termine il sistema sarà come prima.\n\
         Un secondo Ctrl-C esce subito — in quel caso il sistema resta a metà e si \
         ripulisce con `sudo invok rollback`."
            .to_string(),
    )
}
