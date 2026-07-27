//! Pattern `PreState` e persistenza dello stato su disco.
//!
//! Implementa l'invariante 4 di `CLAUDE.md`: `completed` + snapshot vanno
//! scritti su `/opt/odoo/.installer-state.json` (owned root, `0600`).
//!
//! # Stato di fatto: la persistenza non ha ancora un consumatore
//!
//! Il file di stato viene **scritto** dopo ogni step riuscito ([`InstallState::save`],
//! chiamata da [`crate::engine::Installer::execute_with_reporter`]) e
//! **rimosso** a fine installazione riuscita ([`InstallState::clear`], chiamata
//! da `main`). Ma **nessuno lo rilegge**: `main` costruisce sempre un
//! [`crate::engine::Installer`] con stato vuoto e non chiama mai
//! [`InstallState::load`].
//!
//! Concretamente: il rollback dell'installer è **solo in-process** — annulla gli
//! step completati nella *stessa* esecuzione. Il resume (o il rollback) a
//! partire dallo stato persistito **non è implementato**; è pianificato (fase
//! R4 / proposta B1 dell'audit). Finché non lo è, un processo ucciso
//! brutalmente (SIGKILL, power-loss) lascia gli artefatti a metà: lo stato su
//! disco li descrive, ma nessun codice li usa per ripulirli.
//!
//! Il path è configurabile (vedi [`crate::context::Context::state_path`]) così
//! i test girano senza root e senza toccare il sistema.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::StepError;

/// Percorso di default del file di stato in produzione (root, `0600`).
pub const DEFAULT_STATE_PATH: &str = "/opt/odoo/.installer-state.json";

/// Modalità del file di stato: leggibile/scrivibile solo dal proprietario.
const STATE_FILE_MODE: u32 = 0o600;

/// Stato preesistente di un artefatto rispetto all'installer.
///
/// È la sola fonte di verità per l'undo (invariante 1): `undo` agisce **solo**
/// se lo step è `completed` E `PreState == CreatedByUs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PreState {
    /// `run()` non ancora eseguito → nessun undo.
    #[default]
    Untracked,
    /// L'artefatto esisteva già prima di noi → undo NO-OP (non è nostro da
    /// distruggere).
    Preexisting,
    /// Creato da noi → undo rimuove.
    CreatedByUs,
}

/// Record di uno step completato con successo, persistito su disco.
///
/// Lo `snapshot` è un valore JSON opaco al motore: ogni step serializza qui il
/// proprio `PreState` (o una struttura più ricca). È l'informazione che
/// *servirà* a ricostruire l'undo in un'esecuzione successiva — oggi viene
/// scritta ma non ancora riletta (vedi la nota nel doc del modulo).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepRecord {
    pub name: String,
    pub snapshot: serde_json::Value,
}

/// Stato completo dell'installazione persistito su disco.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InstallState {
    /// Step completati, in ordine di esecuzione. Il rollback li percorre in
    /// ordine inverso (invariante 2).
    pub completed: Vec<StepRecord>,
}

impl InstallState {
    /// Carica lo stato dal file. Un file assente equivale a stato vuoto: è la
    /// condizione normale di una prima esecuzione, non un errore.
    ///
    /// **Non è ancora usata dal flusso di installazione**: è il mattone su cui
    /// poggerà il resume/rollback da stato persistito (fase R4). Oggi la
    /// esercitano solo i test.
    pub fn load(path: &Path) -> Result<Self, StepError> {
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
                StepError::io(
                    path,
                    std::io::Error::new(std::io::ErrorKind::InvalidData, e),
                )
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(StepError::io(path, e)),
        }
    }

    /// Scrive lo stato su disco creando il file con permessi `0600` fin dalla
    /// creazione (nessuna finestra a permessi larghi). Se il file esisteva già,
    /// i permessi vengono comunque forzati a `0600`.
    pub fn save(&self, path: &Path) -> Result<(), StepError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|e| StepError::io(parent, e))?;
            }
        }

        let json = serde_json::to_vec_pretty(self).map_err(|e| {
            StepError::io(
                path,
                std::io::Error::new(std::io::ErrorKind::InvalidData, e),
            )
        })?;

        // `mode()` applica i permessi solo alla *creazione* del file.
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(STATE_FILE_MODE)
            .open(path)
            .map_err(|e| StepError::io(path, e))?;
        file.write_all(&json).map_err(|e| StepError::io(path, e))?;

        // Se il file preesisteva con permessi diversi, `mode()` non li tocca:
        // forziamo `0600` esplicitamente.
        fs::set_permissions(path, fs::Permissions::from_mode(STATE_FILE_MODE))
            .map_err(|e| StepError::io(path, e))?;

        Ok(())
    }

    /// Rimuove il file di stato. Idempotente: un file già assente non è errore.
    ///
    /// Chiamata da `main` a fine installazione riuscita: lo stato descrive
    /// un'installazione *in corso*, quindi a successo avvenuto va tolto di mezzo
    /// per non lasciare un file stantìo che il resume di R4 scambierebbe per
    /// un'installazione da riprendere.
    pub fn clear(path: &Path) -> Result<(), StepError> {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(StepError::io(path, e)),
        }
    }

    /// Aggiunge un record di step completato allo stato in memoria.
    pub fn record(&mut self, record: StepRecord) {
        self.completed.push(record);
    }
}
