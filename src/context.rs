//! Il [`Context`]: configurazione risolta passata a ogni step.
//!
//! Gli step leggono **solo** da qui: non sanno se i valori vengano da flag CLI,
//! da un file `.env` o da prompt interattivi (la risoluzione avviene in
//! [`crate::config`]). Questo disaccoppiamento è ciò che permette allo stesso
//! installer di girare interattivo o non-interattivo con un unico flusso.

use std::path::PathBuf;

use crate::config::ResolvedConfig;
use crate::secret::Secret;

/// Configurazione risolta dell'installazione + stato runtime del motore.
///
/// La `admin_passwd` è un [`Secret`]: il suo `Debug` è redatto, quindi un
/// eventuale log di `Context` non espone la password.
#[derive(Debug, Clone, Default)]
pub struct Context {
    /// Versione Odoo completa (es. `"18.0"`).
    pub odoo_version: String,
    /// Versione Odoo short (es. `"18"`), per nomi di file/unit.
    pub odoo_version_short: String,
    /// Utente di sistema che possiederà l'installazione.
    pub odoo_user: String,
    /// Utente PostgreSQL.
    pub db_user: String,
    /// Home dell'installazione: costante `/opt/odoo`.
    pub odoo_home: PathBuf,
    /// Porta HTTP di Odoo.
    pub port: u16,
    /// Nome del database Odoo.
    pub db_name: String,
    /// Directory di installazione (sotto `odoo_home`).
    pub install_dir: PathBuf,
    /// Password admin Odoo (redatta nei log).
    pub admin_passwd: Secret,
    /// Se configurare Nginx come reverse proxy.
    pub with_nginx: bool,
    /// Se `true`, `run`/`undo` non devono mutare il sistema né persistere stato.
    pub dry_run: bool,
    /// Percorso del file di stato persistito. Configurabile per i test.
    pub state_path: PathBuf,
}

impl Context {
    /// Costruisce il `Context` a partire dalla config risolta, aggiungendo lo
    /// stato runtime del motore (`dry_run`, `state_path`).
    pub fn from_resolved(config: ResolvedConfig, dry_run: bool, state_path: PathBuf) -> Self {
        Context {
            odoo_version: config.version,
            odoo_version_short: config.version_short,
            odoo_user: config.odoo_user,
            db_user: config.db_user,
            odoo_home: config.odoo_home,
            port: config.port,
            db_name: config.db_name,
            install_dir: config.install_dir,
            admin_passwd: config.admin_passwd,
            with_nginx: config.with_nginx,
            dry_run,
            state_path,
        }
    }

    /// Override del path del file di stato (usato dai test e dal resume).
    pub fn with_state_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.state_path = path.into();
        self
    }
}
