//! Lock esclusivo anti-concorrenza (gap G5): impedisce due esecuzioni
//! simultanee dell'installer.
//!
//! Usa `flock` (advisory, via `nix`) su un file di lock. Il rilascio è
//! garantito da un guard RAII: avviene su successo, errore o panic (Drop).
//! Vive in `main`, non nel trait `Step`.

use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use nix::fcntl::{Flock, FlockArg};

use crate::error::StepError;

/// Percorso di default del lock file (owned root).
///
/// **Fuori da `/opt/odoo`, e non è un dettaglio estetico** (A-V3-2). Finché il
/// lock viveva in `/opt/odoo/.installer.lock`, acquisirlo significava creare
/// `/opt/odoo` — e il lock si acquisisce **prima** del motore. `PrepareOptRoot`
/// trovava quindi la directory già presente, la marcava `Preexisting`, e il suo
/// undo — l'unico codice che rende reversibile la creazione di `/opt/odoo` —
/// non poteva attivarsi in nessuna esecuzione reale. `/run` è tmpfs: esiste
/// sempre, sparisce al reboot (semantica giusta per un lock) e non appartiene
/// al perimetro che l'installer deve saper rimuovere.
pub const DEFAULT_LOCK_PATH: &str = "/run/odoo-installer.lock";

/// Modalità del lock file: `0600`, come gli altri file dell'installer (stato,
/// temporanei di config). Il file è vuoto e serve solo al `flock`, ma non c'è
/// ragione di lasciarlo `0666 & ~umask` mentre tutto il resto è privato.
const LOCK_FILE_MODE: u32 = 0o600;

/// Guard del lock: mantiene il `flock`. Il rilascio avviene al `Drop`.
pub struct LockGuard {
    _flock: Flock<std::fs::File>,
}

/// Acquisisce un lock esclusivo non-bloccante su `path`.
///
/// Se il lock è già tenuto da un'altra esecuzione → errore chiaro, **senza
/// mutare nulla**.
///
/// **Non crea la directory genitrice**, e questa è la proprietà che conta
/// (A-V3-2). Prendere un lock è un'operazione di coordinamento: non deve far
/// nascere directory, tanto meno una che appartiene al perimetro reversibile e
/// che un altro step deve poter registrare come propria. Se il genitore non
/// esiste, `open` fallisce nominando il percorso — meglio un errore che una
/// directory creata di soppiatto da chi non la possiede.
pub fn acquire(path: &Path) -> Result<LockGuard, StepError> {
    // `mode()` vale solo alla **creazione**: un lock file già esistente conserva
    // i suoi permessi. Non li forziamo — `flock` opera sul descrittore, i
    // permessi non influenzano il locking, e riscrivere i permessi di un file
    // altrui non è compito di questa funzione.
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
            "un'altra installazione è in corso (lock su {}). Attendi il termine o rimuovi il lock \
             se sei certo che nessun'altra esecuzione sia attiva.",
            path.display()
        ))),
    }
}
