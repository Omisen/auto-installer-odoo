//! Il [`Context`]: configurazione risolta passata a ogni step.
//!
//! Gli step leggono **solo** da qui: non sanno se i valori vengano da prompt
//! interattivi (`inquire`, Fase 11), da un file `.env` o da flag CLI (Fase 1).
//! Questo disaccoppiamento è ciò che permette allo stesso installer di girare
//! interattivo o non-interattivo con un unico flusso.

use std::path::PathBuf;

/// Configurazione risolta dell'installazione.
///
/// In Fase 0 i campi sono un set minimo di placeholder costruito a mano; il
/// parsing `.env`/CLI arriva in Fase 1. Il campo [`Context::state_path`] rende
/// configurabile la posizione del file di stato, così i test non toccano
/// `/opt/odoo` né richiedono root.
#[derive(Debug, Clone)]
pub struct Context {
    /// Versione di Odoo da installare (es. `"18.0"`).
    pub odoo_version: String,
    /// Utente di sistema che possiederà l'installazione.
    pub odoo_user: String,
    /// Home dell'installazione (tipicamente `/opt/odoo`).
    pub odoo_home: PathBuf,
    /// Nome del database Odoo.
    pub db_name: String,
    /// Se `true`, `run`/`undo` non devono mutare il sistema né persistere stato.
    pub dry_run: bool,
    /// Percorso del file di stato persistito. Configurabile per i test.
    pub state_path: PathBuf,
}

impl Context {
    /// Costruisce un `Context` usando il path di stato di default
    /// ([`crate::state::DEFAULT_STATE_PATH`]).
    pub fn new(
        odoo_version: impl Into<String>,
        odoo_user: impl Into<String>,
        odoo_home: impl Into<PathBuf>,
        db_name: impl Into<String>,
        dry_run: bool,
    ) -> Self {
        Context {
            odoo_version: odoo_version.into(),
            odoo_user: odoo_user.into(),
            odoo_home: odoo_home.into(),
            db_name: db_name.into(),
            dry_run,
            state_path: PathBuf::from(crate::state::DEFAULT_STATE_PATH),
        }
    }

    /// Override del path del file di stato (usato dai test e dal resume).
    pub fn with_state_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.state_path = path.into();
        self
    }
}
