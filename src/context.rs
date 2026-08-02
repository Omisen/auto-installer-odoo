//! Il [`Context`]: configurazione risolta passata a ogni step.
//!
//! Gli step leggono **solo** da qui: non sanno se i valori vengano da flag CLI,
//! da un file `.env` o da prompt interattivi (la risoluzione avviene in
//! [`crate::config`]). Questo disaccoppiamento è ciò che permette allo stesso
//! installer di girare interattivo o non-interattivo con un unico flusso.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::checks::OsInfo;
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
    /// Password del ruolo PostgreSQL (redatta nei log); vuota = peer auth.
    pub db_password: Secret,
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
    /// Logfile Odoo; `None` = log su journal/stdout. Determina se
    /// [`crate::steps::setup_log_dir`] crea una directory o è no-op.
    pub odoo_logfile: Option<PathBuf>,
    /// Se configurare Nginx come reverse proxy.
    pub with_nginx: bool,
    /// Nome server/dominio del vhost Nginx (default `_`).
    pub nginx_server_name: String,
    /// Se abilitare SSL nel vhost e aprire la 443 sul firewall.
    /// Apre la 443 sul firewall; non abilita TLS (A-V3-6).
    pub nginx_open_https_port: bool,
    /// Se `true`, `run`/`undo` non devono mutare il sistema né persistere stato.
    pub dry_run: bool,
    /// Se `true`, il rollback purga anche pacchetti che di norma lascerebbe
    /// (utility bootstrap comuni, PostgreSQL). Default `false`.
    pub aggressive_rollback: bool,
    /// Percorso del file di stato persistito. Configurabile per i test.
    pub state_path: PathBuf,
    /// Info OS rilevate dai preflight checks; popolate in `main` dopo `check_os`
    /// e usate dalle fasi successive (es. wkhtmltopdf). `None` finché non note.
    pub os_info: Option<OsInfo>,
    /// Utente che ha lanciato `sudo` (`SUDO_USER`), proprietario del
    /// control-script e del `.bashrc`. Popolato in `main`. `None` se assente.
    pub sudo_user: Option<String>,
    /// Risultato condiviso pubblicato da `CreateDatabase` e letto da
    /// `InitializeOdooDatabase`: `true` = il DB è nostro (non preesistente),
    /// quindi è lecito inizializzarne lo schema. **Default `false`** = rifiuta
    /// (safe: senza propagazione, l'init è vietato). Interior mutability così
    /// può essere impostato da uno step che riceve `&Context`.
    pub db_created_by_us: Arc<AtomicBool>,
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
            db_password: config.db_password,
            odoo_home: config.odoo_home,
            port: config.port,
            db_name: config.db_name,
            install_dir: config.install_dir,
            admin_passwd: config.admin_passwd,
            odoo_logfile: config.odoo_logfile,
            with_nginx: config.with_nginx,
            nginx_server_name: config.nginx_server_name,
            nginx_open_https_port: config.nginx_open_https_port,
            dry_run,
            aggressive_rollback: false,
            state_path,
            os_info: None,
            sudo_user: None,
            db_created_by_us: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Override del path del file di stato (usato dai test; lo userà il resume
    /// quando sarà implementato — vedi [`crate::state`]).
    pub fn with_state_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.state_path = path.into();
        self
    }
}
