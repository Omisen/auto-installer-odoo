//! Definizione degli argomenti da riga di comando (`clap`, derive).
//!
//! Ogni parametro sovrascrivibile è un `Option<T>`: `Some` = passato
//! esplicitamente dall'utente (a qualsiasi valore), `None` = non passato. Questo
//! sostituisce i booleani `CLI_*_SET` del Bash originale portando la stessa
//! informazione in modo tipizzato, ed è ciò che permette la cascata di priorità
//! CLI → `.env` → interattivo → default (vedi [`crate::config`]).
//!
//! # Sottocomandi (R4)
//!
//! `odoo-installer` **senza** sottocomando installa, esattamente come prima:
//! è l'uso documentato e non cambia. `odoo-installer rollback` (alias
//! `uninstall`) annulla un'installazione a partire dallo stato persistito.
//! Il sottocomando è `Option`, quindi la forma senza resta valida.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// Installer Odoo con rollback chirurgico.
#[derive(Parser, Debug)]
#[command(
    name = "odoo-installer",
    about = "Installer Odoo (16/17/18/19) con rollback chirurgico",
    // Niente auto `--version` di clap: `--version` qui è la versione di Odoo.
    disable_version_flag = true
)]
pub struct Cli {
    /// Sottocomando. Assente = installazione (comportamento storico).
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Versione Odoo da installare (16|17|18|19 oppure 16.0..19.0).
    #[arg(long, value_name = "VERSION")]
    pub version: Option<String>,

    /// Utente di sistema per Odoo (default: odoo).
    #[arg(long, value_name = "USER")]
    pub odoo_user: Option<String>,

    /// Utente PostgreSQL (default: uguale a --odoo-user).
    #[arg(long, value_name = "USER")]
    pub db_user: Option<String>,

    /// Password del ruolo PostgreSQL. Vuota/assente → autenticazione peer.
    #[arg(long, value_name = "PASS")]
    pub db_password: Option<String>,

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

    /// Nome server/dominio per il vhost Nginx (default: `_` catch-all).
    #[arg(long, value_name = "NAME")]
    pub server_name: Option<String>,

    /// Abilita SSL nel vhost Nginx e apre la porta 443 sul firewall.
    #[arg(long)]
    pub enable_ssl: bool,

    /// Percorso del logfile Odoo. Se assente, Odoo logga su journal/stdout.
    #[arg(long, value_name = "FILE")]
    pub logfile: Option<PathBuf>,

    /// Carica variabili da un file .env (parsing dichiarativo, mai eseguito).
    #[arg(long, value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// Non muta il sistema: risolve la config e simula, senza effetti reali.
    #[arg(long)]
    pub dry_run: bool,

    /// Rollback aggressivo: purga anche le utility comuni (git/curl/wget/…) e i
    /// pacchetti pesanti. Di default il rollback le lascia installate.
    #[arg(long)]
    pub aggressive_rollback: bool,
}

/// I sottocomandi disponibili.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Annulla un'installazione leggendo lo stato persistito: rimuove ciò che
    /// l'installer ha creato, lasciando intatto ciò che era già sulla macchina.
    ///
    /// Serve sia a disinstallare un'istanza funzionante, sia a ripulire i resti
    /// di un'installazione interrotta (Ctrl-C, crash, spegnimento).
    #[command(alias = "uninstall")]
    Rollback(RollbackArgs),
}

/// Opzioni di `odoo-installer rollback`.
#[derive(Args, Debug)]
pub struct RollbackArgs {
    /// File di stato da consumare (default: /var/lib/odoo-installer/state.json,
    /// con ripiego sul percorso storico /opt/odoo/.installer-state.json).
    #[arg(long, value_name = "FILE")]
    pub state: Option<PathBuf>,

    /// Mostra cosa verrebbe annullato senza toccare il sistema.
    #[arg(long)]
    pub dry_run: bool,

    /// Rollback aggressivo: purga anche PostgreSQL/Nginx installati da noi e le
    /// utility comuni. Di default restano installati (stop + disable bastano).
    #[arg(long)]
    pub aggressive_rollback: bool,

    /// Non chiedere conferma. Necessario in esecuzioni non interattive: senza
    /// TTY e senza questo flag il comando si ferma invece di procedere alla
    /// cieca su un'operazione distruttiva.
    #[arg(short = 'y', long)]
    pub yes: bool,
}
