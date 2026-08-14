//! exclusive lock against concurrent runs (gap G5).
//!
//! advisory `flock` (via `nix`) on a lock file, released by an RAII guard: on
//! success, on error and on panic (`Drop`). lives in `main`, not in `Step`.

use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use nix::fcntl::{Flock, FlockArg};

use crate::error::StepError;

/// default lock path, owned by root.
///
/// **outside `/opt/odoo`** (A-V3-2): the lock is taken *before* the engine, so
/// living there created the directory and left `PrepareOptRoot` marking it
/// `Preexisting` — its undo could never fire in a real run. `/run` is tmpfs:
/// always present, cleared on reboot, outside the reversible perimeter.
pub const DEFAULT_LOCK_PATH: &str = "/run/invok.lock";

/// `0600`, like every other file the installer creates.
const LOCK_FILE_MODE: u32 = 0o600;

/// holds the `flock`; releases it on `Drop`.
pub struct LockGuard {
    _flock: Flock<std::fs::File>,
}

/// acquires an exclusive non-blocking lock on `path`.
///
/// the parent directory is deliberately **not** created (A-V3-2): taking a lock
/// is coordination, and must not bring into existence a directory another step
/// has to claim as its own.
///
/// # errors
///
/// [`StepError::Precondition`] when another run holds the lock, without
/// mutating anything, and [`StepError::io`] when `path` cannot be opened.
pub fn acquire(path: &Path) -> Result<LockGuard, StepError> {
    // `mode()` applies at creation only; an existing lock file keeps its own
    // permissions. `flock` works on the descriptor, so they do not matter.
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(LOCK_FILE_MODE)
        .open(path)
        .map_err(|e| StepError::io(path, e))?;

    match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
        Ok(flock) => Ok(LockGuard { _flock: flock }),
        Err(_) => Err(StepError::Precondition(format!(
            "another installation is in progress (lock on {}). wait for it to finish, or \
             remove the lock if you are certain no other run is active.",
            path.display()
        ))),
    }
}
