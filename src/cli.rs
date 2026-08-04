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
    version = crate::INSTALLER_VERSION,
    // Niente auto `--version` di clap: `--version` qui è la versione di Odoo.
    // Il flag automatico resta spento, ma la versione **si può chiedere**: vedi
    // `installer_version` qui sotto (A-V3-16).
    disable_version_flag = true
)]
pub struct Cli {
    /// Sottocomando. Assente = installazione (comportamento storico).
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Versione Odoo da installare (16|17|18|19 oppure 16.0..19.0).
    #[arg(long, value_name = "VERSION")]
    pub version: Option<String>,

    /// Stampa la versione **dell'installer** ed esce.
    ///
    /// Nome lungo esplicito e non `--version`, che qui è già la versione di Odoo
    /// e non si tocca: rinominarla romperebbe script e `.env` in campo, e questo
    /// progetto le rinomine le fa mantenendo l'alias (R12), non spostando il
    /// significato di un flag esistente. La forma breve `-V` resta quella che
    /// chiunque prova per prima.
    #[arg(short = 'V', long = "installer-version", action = clap::ArgAction::Version)]
    pub installer_version: Option<bool>,

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

    /// Apre la porta 443 sul firewall, in vista di TLS.
    ///
    /// **Non configura TLS**: il vhost generato ascolta solo sulla 80. I
    /// certificati e il blocco `server` su 443 li mette `certbot --nginx`, che
    /// riscrive il vhost da sé. Questo flag serve ad avere la porta già aperta
    /// quando lo farai.
    ///
    /// Si chiamava `--enable-ssl`, nome che prometteva ciò che non faceva
    /// (A-V3-6). Il vecchio nome resta accettato per non rompere gli script.
    #[arg(long, alias = "enable-ssl")]
    pub open_https_port: bool,

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

    /// Installa anche se esiste già un manifesto, mettendolo da parte invece di
    /// sovrascriverlo. Serve a reinstallare sopra un'istanza esistente: il
    /// manifesto precedente viene rinominato, mai cancellato, perché resta
    /// l'unica traccia di quali artefatti quell'installazione aveva creato.
    #[arg(long)]
    pub force: bool,
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
