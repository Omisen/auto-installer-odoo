//! Lock esclusivo anti-concorrenza (gap G5): impedisce due esecuzioni
//! simultanee dell'installer.
//!
//! Usa `flock` (advisory, via `nix`) su un file di lock. Il rilascio è
//! garantito da un guard RAII: avviene su successo, errore o panic (Drop).
//! Vive in `main`, non nel trait `Step`.

use std::fs::OpenOptions;
use std::path::Path;

use nix::fcntl::{Flock, FlockArg};

use crate::error::StepError;

/// Percorso di default del lock file (owned root).
pub const DEFAULT_LOCK_PATH: &str = "/opt/odoo/.installer.lock";

/// Guard del lock: mantiene il `flock`. Il rilascio avviene al `Drop`.
pub struct LockGuard {
    _flock: Flock<std::fs::File>,
}

/// Acquisisce un lock esclusivo non-bloccante su `path`.
///
/// Se il lock è già tenuto da un'altra esecuzione → errore chiaro, **senza
/// mutare nulla**.
pub fn acquire(path: &Path) -> Result<LockGuard, StepError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| StepError::io(parent, e))?;
        }
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(|e| StepError::io(path, e))?;

    match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
        Ok(flock) => Ok(LockGuard { _flock: flock }),
        Err(_) => Err(StepError::Precondition(format!(
            "un'altra installazione è in corso (lock su {}). Attendi il termine o rimuovi il lock \
             se sei certo che nessun'altra esecuzione sia attiva.",
            path.display()
        ))),
    }
}
