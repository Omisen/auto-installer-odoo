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
use odoo_installer::engine::Installer;
use odoo_installer::prompt;
use odoo_installer::state::DEFAULT_STATE_PATH;
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
use odoo_installer::steps::prepare_opt_root::PrepareOptRoot;
use odoo_installer::steps::setup_log_dir::SetupLogDir;
use odoo_installer::steps::setup_postgres::SetupPostgres;
use odoo_installer::steps::setup_systemd::SetupSystemd;

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
    let mut ctx = Context::from_resolved(resolved, cli.dry_run, state_path);
    ctx.aggressive_rollback = cli.aggressive_rollback;

    print_configuration(&ctx);

    // 1) Preflight checks NON mutanti: falliscono prima di ogni mutazione.
    let os_info = run_preflight_checks(&ctx)?;
    ctx.os_info = Some(os_info);

    // 2) Step reversibili, in ordine: prima la dir, poi l'utente che ne diventa
    //    owner, poi l'eventuale log dir che ha bisogno dell'utente. Il rollback
    //    li annulla in ordine inverso (log dir → utente → dir).
    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(PrepareOptRoot::new()),
        Box::new(CreateOdooUser::new()),
        Box::new(SetupLogDir::new()),
        Box::new(AptPackagesStep::bootstrap()),
        Box::new(AptPackagesStep::odoo_dependencies()),
        Box::new(InstallWkhtmltopdf::new()),
        // PostgreSQL: ordine cruciale per il rollback inverso —
        // undo: CreateDatabase (drop DB) → CreateDbRole (drop ruolo) → SetupPostgres (stop/disable).
        Box::new(SetupPostgres::new()),
        Box::new(CreateDbRole::new()),
        Box::new(CreateDatabase::new()),
        // Sorgenti Odoo: clone → venv → pip. Rollback inverso: pip (no-op) →
        // venv (rm -rf sandbox) → clone (rm -rf odoo + contenitore se vuoto).
        Box::new(CloneOdooRepo::new()),
        Box::new(CreateVirtualenv::new()),
        Box::new(InstallPythonRequirements::new()),
        // Config + init schema. L'undo di init è no-op: la pulizia dello schema
        // è coperta dal dropdb di CreateDatabase (più a valle nella catena inversa).
        Box::new(GenerateConfig::new()),
        Box::new(InitializeOdooDatabase::new()),
        // Servizio systemd. Undo: stop → disable → rm → daemon-reload.
        Box::new(SetupSystemd::new()),
    ];
    let mut installer = Installer::new();
    installer.execute(&mut steps, &ctx).map_err(|e| {
        // Il rollback è già stato eseguito dentro `execute`.
        anyhow!(e)
    })?;

    tracing::info!("preparazione completata");
    Ok(())
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
