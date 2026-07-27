//! Entry point (Fase 1): risoluzione della configurazione e riepilogo.
//!
//! Flusso: parse CLI → carica `.env` (se presente) → prompt interattivi (se
//! TTY) → risolvi la cascata → conferma password 'admin' se serve → costruisci
//! il [`Context`] → stampa il riepilogo e termina. **Nessuno step di sistema
//! viene eseguito**: quelli iniziano dalla Fase 2.

use std::path::PathBuf;

use anyhow::{anyhow, bail, Result};
use clap::Parser;

use odoo_installer::checks::{self, OsInfo};
use odoo_installer::cli::Cli;
use odoo_installer::config::{self, AdminConfirm, RawConfig, ResolvedConfig};
use odoo_installer::context::Context;
use odoo_installer::engine::{dry_run_plan, Installer};
use odoo_installer::progress::{IndicatifReporter, LogReporter, ProgressReporter};
use odoo_installer::prompt;
use odoo_installer::state::{InstallState, DEFAULT_STATE_PATH};
use odoo_installer::step::Step;
use odoo_installer::steps::apt_packages::AptPackagesStep;
use odoo_installer::steps::clone_odoo_repo::CloneOdooRepo;
use odoo_installer::steps::create_database::CreateDatabase;
use odoo_installer::steps::create_db_role::CreateDbRole;
use odoo_installer::steps::create_odoo_user::CreateOdooUser;
use odoo_installer::steps::create_virtualenv::CreateVirtualenv;
use odoo_installer::steps::generate_config::GenerateConfig;
use odoo_installer::steps::initialize_odoo_database::InitializeOdooDatabase;
use odoo_installer::steps::install_python_requirements::InstallPythonRequirements;
use odoo_installer::steps::install_wkhtmltopdf::InstallWkhtmltopdf;
use odoo_installer::steps::nginx_enable_site::NginxEnableSite;
use odoo_installer::steps::nginx_firewall::NginxFirewall;
use odoo_installer::steps::nginx_install::NginxInstall;
use odoo_installer::steps::nginx_reload::NginxReload;
use odoo_installer::steps::nginx_write_config::NginxWriteConfig;
use odoo_installer::steps::patch_bashrc::PatchBashrc;
use odoo_installer::steps::prepare_opt_root::PrepareOptRoot;
use odoo_installer::steps::setup_log_dir::SetupLogDir;
use odoo_installer::steps::setup_postgres::SetupPostgres;
use odoo_installer::steps::setup_systemd::SetupSystemd;
use odoo_installer::steps::write_control_script::WriteControlScript;

fn main() -> Result<()> {
    let cli = Cli::parse();
    // Logging TTY + file (degrada senza root; niente file in dry-run). Il guard
    // va tenuto in vita per tutta l'esecuzione.
    let _log_guard = odoo_installer::logging::init(cli.dry_run);

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
    let resolved = ResolvedConfig::resolve(&cli_raw, &env_raw, &prompted, interactive)
        .map_err(|e| anyhow!(e))?;

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
    let mut ctx = Context::from_resolved(resolved, cli.dry_run, state_path);
    ctx.aggressive_rollback = cli.aggressive_rollback;
    ctx.sudo_user = std::env::var("SUDO_USER").ok().filter(|s| !s.is_empty());

    print_configuration(&ctx);

    let mut steps = build_steps();

    // Reporter: barra `indicatif` solo con TTY interattivo e installazione reale;
    // altrimenti solo log. Il motore dipende dall'astrazione, non da indicatif.
    let reporter: Box<dyn ProgressReporter> = if interactive && !ctx.dry_run {
        Box::new(IndicatifReporter::new(steps.len()))
    } else {
        Box::new(LogReporter)
    };

    // --- dry-run: mostra il piano, non muta nulla, non persiste stato ---------
    if ctx.dry_run {
        println!("=== PIANO (dry-run) — nessuna modifica al sistema ===");
        dry_run_plan(&mut steps, &ctx, reporter.as_ref());
        println!("=== fine piano (dry-run) ===");
        return Ok(());
    }

    // Conferma finale interattiva prima di mutare il sistema.
    if interactive && !prompt::confirm("Procedere con l'installazione?")? {
        bail!("Installazione annullata dall'utente.");
    }

    // Preflight checks NON mutanti: falliscono prima di ogni mutazione.
    let os_info = run_preflight_checks(&ctx)?;
    ctx.os_info = Some(os_info);

    // Lock esclusivo: impedisce due installazioni simultanee. Il guard rilascia
    // il lock al Drop (successo, errore o panic). Acquisito dopo i check e prima
    // di ogni mutazione.
    let _lock = odoo_installer::lockfile::acquire(std::path::Path::new(
        odoo_installer::lockfile::DEFAULT_LOCK_PATH,
    ))
    .map_err(|e| anyhow!(e))?;

    let mut installer = Installer::new();
    installer
        .execute_with_reporter(&mut steps, &ctx, reporter.as_ref())
        .map_err(|e| {
            // Il rollback è già stato eseguito dentro `execute`.
            anyhow!(e)
        })?;

    // Installazione riuscita: lo stato descrive un'installazione *in corso*,
    // quindi va rimosso. Altrimenti resterebbe un file stantìo che il resume
    // (fase R4) scambierebbe per un'installazione da riprendere. Fallire qui non
    // annulla un'installazione riuscita: si segnala e si prosegue.
    // In dry-run non si arriva qui (return anticipato) e nulla è stato scritto.
    if let Err(e) = InstallState::clear(&ctx.state_path) {
        tracing::warn!(
            path = %ctx.state_path.display(),
            error = %e,
            "rimozione del file di stato fallita: rimuovilo a mano per evitare confusione"
        );
    }

    tracing::info!("preparazione completata");
    Ok(())
}

/// Costruisce la sequenza di produzione degli step, nell'ordine di esecuzione.
/// Il rollback li annulla in ordine inverso.
fn build_steps() -> Vec<Box<dyn Step>> {
    vec![
        Box::new(PrepareOptRoot::new()),
        Box::new(CreateOdooUser::new()),
        Box::new(SetupLogDir::new()),
        Box::new(AptPackagesStep::bootstrap()),
        Box::new(AptPackagesStep::odoo_dependencies()),
        Box::new(InstallWkhtmltopdf::new()),
        // PostgreSQL: undo inverso CreateDatabase → CreateDbRole → SetupPostgres.
        Box::new(SetupPostgres::new()),
        Box::new(CreateDbRole::new()),
        Box::new(CreateDatabase::new()),
        // Sorgenti: clone → venv → pip (undo pip no-op; venv rm; clone rm).
        Box::new(CloneOdooRepo::new()),
        Box::new(CreateVirtualenv::new()),
        Box::new(InstallPythonRequirements::new()),
        // Config + init schema (undo init no-op: pulizia dal dropdb di Fase 5).
        Box::new(GenerateConfig::new()),
        Box::new(InitializeOdooDatabase::new()),
        // Servizio systemd (undo: stop → disable → rm → daemon-reload).
        Box::new(SetupSystemd::new()),
        // Nginx (opzionale, gated): install → vhost → enable → firewall → reload.
        Box::new(NginxInstall::new()),
        Box::new(NginxWriteConfig::new()),
        Box::new(NginxEnableSite::new()),
        Box::new(NginxFirewall::new()),
        Box::new(NginxReload::new()),
        // Comando helper `odoo` + patch PATH nel .bashrc dell'utente.
        Box::new(WriteControlScript::new()),
        Box::new(PatchBashrc::new()),
    ]
}

/// Esegue i preflight checks non mutanti nell'ordine del Bash. Ritorna le info
/// OS da propagare nel [`Context`]. Un fallimento ferma tutto prima di qualsiasi
/// mutazione.
fn run_preflight_checks(ctx: &Context) -> Result<OsInfo> {
    checks::check_root().map_err(|e| anyhow!(e))?;
    checks::check_sudo_user().map_err(|e| anyhow!(e))?;
    let os_info = checks::check_os().map_err(|e| anyhow!(e))?;

    let required_gb = std::env::var("MIN_DISK_GB")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(checks::DEFAULT_MIN_DISK_GB);
    checks::check_disk(&ctx.odoo_home, required_gb).map_err(|e| anyhow!(e))?;

    checks::check_ports(ctx.port, ctx.with_nginx).map_err(|e| anyhow!(e))?;
    checks::check_commands().map_err(|e| anyhow!(e))?;
    Ok(os_info)
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
    println!(
        "  Nginx         : {}",
        if ctx.with_nginx { "attivo" } else { "no" }
    );
    println!("  Admin passwd  : {admin_line}");
    if ctx.dry_run {
        println!("  Modalità      : dry-run (nessuna mutazione)");
    }
    println!("================================================================");
    println!();
}
