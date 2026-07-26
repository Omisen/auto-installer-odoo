//! Definizione degli argomenti da riga di comando (`clap`, derive).
//!
//! Ogni parametro sovrascrivibile è un `Option<T>`: `Some` = passato
//! esplicitamente dall'utente (a qualsiasi valore), `None` = non passato. Questo
//! sostituisce i booleani `CLI_*_SET` del Bash originale portando la stessa
//! informazione in modo tipizzato, ed è ciò che permette la cascata di priorità
//! CLI → `.env` → interattivo → default (vedi [`crate::config`]).

use std::path::PathBuf;

use clap::Parser;

/// Installer Odoo con rollback chirurgico.
#[derive(Parser, Debug)]
#[command(
    name = "odoo-installer",
    about = "Installer Odoo (16/17/18/19) con rollback chirurgico",
    // Niente auto `--version` di clap: `--version` qui è la versione di Odoo.
    disable_version_flag = true
)]
pub struct Cli {
    /// Versione Odoo da installare (16|17|18|19 oppure 16.0..19.0).
    #[arg(long, value_name = "VERSION")]
    pub version: Option<String>,

    /// Utente di sistema per Odoo (default: odoo).
    #[arg(long, value_name = "USER")]
    pub odoo_user: Option<String>,

    /// Utente PostgreSQL (default: uguale a --odoo-user).
    #[arg(long, value_name = "USER")]
    pub db_user: Option<String>,

    /// Porta HTTP di Odoo (default: 8069).
    #[arg(long, value_name = "PORT")]
    pub port: Option<u16>,

    /// Nome del database (default: odoo).
    #[arg(long, value_name = "NAME")]
    pub db_name: Option<String>,

    /// Directory di installazione (deve stare sotto /opt/odoo;
    /// default: /opt/odoo/odoo<versione>).
    #[arg(long, value_name = "DIR")]
    pub install_dir: Option<PathBuf>,

    /// Password admin Odoo (se 'admin' richiede conferma esplicita interattiva).
    #[arg(long, value_name = "PASS")]
    pub admin_passwd: Option<String>,

    /// Configura Nginx come reverse proxy.
    #[arg(long)]
    pub with_nginx: bool,

    /// Percorso del logfile Odoo. Se assente, Odoo logga su journal/stdout.
    #[arg(long, value_name = "FILE")]
    pub logfile: Option<PathBuf>,

    /// Carica variabili da un file .env (parsing dichiarativo, mai eseguito).
    #[arg(long, value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// Non muta il sistema: risolve la config e simula, senza effetti reali.
    #[arg(long)]
    pub dry_run: bool,
}
