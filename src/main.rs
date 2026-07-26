//! Entry point (Fase 1): risoluzione della configurazione e riepilogo.
//!
//! Flusso: parse CLI → carica `.env` (se presente) → prompt interattivi (se
//! TTY) → risolvi la cascata → conferma password 'admin' se serve → costruisci
//! il [`Context`] → stampa il riepilogo e termina. **Nessuno step di sistema
//! viene eseguito**: quelli iniziano dalla Fase 2.

use std::path::PathBuf;

use anyhow::{anyhow, bail, Result};
use clap::Parser;

use odoo_installer::cli::Cli;
use odoo_installer::config::{self, AdminConfirm, RawConfig, ResolvedConfig};
use odoo_installer::context::Context;
use odoo_installer::prompt;
use odoo_installer::state::DEFAULT_STATE_PATH;

fn main() -> Result<()> {
    init_tracing();

    let cli = Cli::parse();
    let interactive = prompt::is_interactive();

    // Sorgenti grezze: CLI e (opzionale) file .env — parsato, mai eseguito.
    let cli_raw = RawConfig::from_cli(&cli);
    let env_raw = match &cli.config {
        Some(path) => {
            let raw = config::parse_env_file(path).map_err(|e| anyhow!(e))?;
            tracing::info!(config = %path.display(), "configurazione .env caricata");
            raw
        }
        None => RawConfig::default(),
    };

    // Prompt interattivi solo per i campi non passati da CLI (vuoto se non TTY).
    let prompted = if interactive {
        prompt::collect(&cli_raw, &env_raw)?
    } else {
        tracing::info!("input interattivo non disponibile: uso CLI, .env e default finali");
        RawConfig::default()
    };

    // Cascata + validazione (pura).
    let resolved =
        ResolvedConfig::resolve(&cli_raw, &env_raw, &prompted, interactive).map_err(|e| anyhow!(e))?;

    // Conferma interattiva della password debole 'admin' (l'hard-stop
    // non-interattivo è già stato applicato dentro `resolve`).
    if let AdminConfirm::ConfirmNeeded =
        config::check_admin_password(resolved.admin_passwd.expose(), interactive)?
    {
        tracing::warn!(
            "La password admin Odoo è impostata al valore debole 'admin': usala solo per demo o ambienti temporanei."
        );
        if !prompt::confirm("Confermi di voler continuare con admin_passwd='admin'?")? {
            bail!("Installazione interrotta: imposta una password admin diversa da 'admin'.");
        }
    }

    let state_path = PathBuf::from(DEFAULT_STATE_PATH);
    let ctx = Context::from_resolved(resolved, cli.dry_run, state_path);

    print_configuration(&ctx);
    Ok(())
}

/// Stampa il riepilogo della configurazione finale (replica di
/// `print_installation_configuration` del Bash). Non stampa mai la password.
fn print_configuration(ctx: &Context) {
    let admin_line = if ctx.admin_passwd.expose() == "admin" {
        "default 'admin' (consentito solo con conferma esplicita; sconsigliato)".to_string()
    } else {
        "personalizzata".to_string()
    };

    println!();
    println!("================================================================");
    println!("Configurazione finale installazione:");
    println!("  Versione Odoo : {}", ctx.odoo_version);
    println!("  Utente Odoo   : {}", ctx.odoo_user);
    println!("  Database      : {}", ctx.db_name);
    println!("  DB user       : {}", ctx.db_user);
    println!("  Porta HTTP    : {}", ctx.port);
    println!("  Install dir   : {}", ctx.install_dir.display());
    println!("  Nginx         : {}", if ctx.with_nginx { "attivo" } else { "no" });
    println!("  Admin passwd  : {admin_line}");
    if ctx.dry_run {
        println!("  Modalità      : dry-run (nessuna mutazione)");
    }
    println!("================================================================");
    println!();
}

/// Inizializza `tracing` verso il TTY, con livello controllabile da `RUST_LOG`
/// (default `info`).
fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = fmt().with_env_filter(filter).try_init();
}
