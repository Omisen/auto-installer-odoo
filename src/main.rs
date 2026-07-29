//! Entry point: dispatch fra i due comandi dell'installer.
//!
//! - **senza sottocomando** → installazione. Flusso: parse CLI → carica `.env`
//!   (se presente) → prompt interattivi (se TTY) → risolvi la cascata → conferma
//!   password 'admin' se serve → costruisci il [`Context`] → preflight → esegui
//!   gli step con rollback automatico in caso di errore.
//! - **`rollback`** (alias `uninstall`) → annulla un'installazione a partire
//!   dallo stato persistito. Vedi [`odoo_installer::rollback`].

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};
use clap::Parser;

use odoo_installer::checks::{self, OsInfo};
use odoo_installer::cli::{Cli, Command, RollbackArgs};
use odoo_installer::config::{self, AdminConfirm, RawConfig, ResolvedConfig};
use odoo_installer::context::Context;
use odoo_installer::engine::{dry_run_plan, Installer};
use odoo_installer::progress::{IndicatifReporter, LogReporter, ProgressReporter};
use odoo_installer::prompt;
use odoo_installer::rollback::{
    self, ConfirmationGate, InstallStatus, RollbackReport, UndoOutcome,
};
use odoo_installer::state::{InstallState, DEFAULT_STATE_PATH};
use odoo_installer::steps;

fn main() -> Result<()> {
    let cli = Cli::parse();
    match &cli.command {
        Some(Command::Rollback(args)) => run_rollback(args),
        // Nessun sottocomando: installazione, come è sempre stato.
        None => run_install(&cli),
    }
}

// --- Installazione -----------------------------------------------------------

fn run_install(cli: &Cli) -> Result<()> {
    // Logging TTY + file (degrada senza root; niente file in dry-run). Il guard
    // va tenuto in vita per tutta l'esecuzione.
    let _log_guard = odoo_installer::logging::init(cli.dry_run);

    let interactive = prompt::is_interactive();

    // Sorgenti grezze: CLI e (opzionale) file .env — parsato, mai eseguito.
    let cli_raw = RawConfig::from_cli(cli);
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

    let mut steps = steps::build_steps();

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

    print_interrupt_notice(&ctx.state_path);

    // Preflight checks NON mutanti: falliscono prima di ogni mutazione.
    let os_info = run_preflight_checks(&ctx)?;
    ctx.os_info = Some(os_info);

    // Lock esclusivo: impedisce due installazioni simultanee. Il guard rilascia
    // il lock al Drop (successo, errore o panic). Acquisito dopo i check e prima
    // di ogni mutazione.
    let _lock =
        odoo_installer::lockfile::acquire(Path::new(odoo_installer::lockfile::DEFAULT_LOCK_PATH))
            .map_err(|e| anyhow!(e))?;

    let mut installer = Installer::new();
    installer
        .execute_with_reporter(&mut steps, &ctx, reporter.as_ref())
        .map_err(|e| {
            // Il rollback in-process è già stato eseguito dentro `execute`.
            anyhow!(e)
        })?;

    // Installazione riuscita: lo stato viene marcato come concluso e **resta sul
    // disco**. Non è un file stantìo — è il manifesto di disinstallazione: dice
    // cosa abbiamo creato e cosa abbiamo trovato già presente, e senza di esso
    // `odoo-installer rollback` non avrebbe modo di rimuovere questa istanza in
    // un secondo momento (A-R5-1). Fallire qui non annulla un'installazione
    // riuscita: si segnala e si prosegue, con il costo dichiarato.
    // In dry-run non si arriva qui (return anticipato) e nulla è stato scritto.
    if let Err(e) = installer.mark_finished(&ctx) {
        tracing::warn!(
            path = %ctx.state_path.display(),
            error = %e,
            "impossibile marcare lo stato come concluso: l'installazione è comunque \
             riuscita, ma `odoo-installer rollback` potrebbe non poterla disinstallare"
        );
    }

    print_install_summary(&ctx);
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

/// Riepilogo di fine installazione: cosa è stato creato e come toccarlo (B2).
///
/// Chiude la metà mancante di B2: il rollback aveva già il suo report da R4,
/// l'installazione riuscita finiva invece con una riga di log. Qui l'utente
/// trova, in un posto solo, dove sono le cose e i due comandi che gli servono.
fn print_install_summary(ctx: &Context) {
    let unit = format!("odoo{}", ctx.odoo_version_short);
    println!();
    println!("================================================================");
    println!("Installazione completata.");
    println!();
    println!(
        "  Odoo {}          http://localhost:{}",
        ctx.odoo_version, ctx.port
    );
    println!("  Servizio         {unit} (systemd)");
    println!("  Utente di sistema {}", ctx.odoo_user);
    println!("  Database         {} (ruolo {})", ctx.db_name, ctx.db_user);
    println!("  Sorgenti         {}", ctx.install_dir.display());
    println!(
        "  Config           {}/odoo{}.conf",
        ctx.install_dir.display(),
        ctx.odoo_version_short
    );
    if let Some(logfile) = &ctx.odoo_logfile {
        println!("  Log Odoo         {}", logfile.display());
    } else {
        println!("  Log Odoo         journal (journalctl -u {unit})");
    }
    if ctx.with_nginx {
        println!("  Nginx            reverse proxy attivo su :80");
    }
    println!();
    println!("  Gestione         odoo start|stop|restart|status   (riapri la shell)");
    println!("  Disinstallazione sudo odoo-installer rollback");
    println!();
    println!(
        "Lo stato in {} è il manifesto di disinstallazione: dice cosa\n\
         è stato creato e cosa era già presente. Non rimuoverlo, serve al rollback.",
        ctx.state_path.display()
    );
    println!("================================================================");
    println!();
}

/// Avviso preventivo su cosa fare se l'installazione viene interrotta.
///
/// Il rollback automatico copre i **fallimenti** di uno step, non le
/// interruzioni: un Ctrl-C uccide il processo prima che la gestione dell'errore
/// giri, e il sistema resta con gli artefatti a metà. Da R4 esiste la via
/// d'uscita — il file di stato è scritto dopo ogni step e `odoo-installer
/// rollback` lo consuma — ma serve saperlo *prima* di premere Ctrl-C, non
/// dopo. Un handler SIGINT che stampi il messaggio al momento giusto è
/// pianificato a parte (vedi audit R4).
fn print_interrupt_notice(state_path: &Path) {
    println!(
        "Nota: se interrompi l'installazione (Ctrl-C) o la macchina si spegne, il sistema \n\
         resta a metà. Per ripulirlo esegui:\n\
     \n    sudo odoo-installer rollback\n\n\
         Lo stato necessario è registrato in {} dopo ogni step.\n",
        state_path.display()
    );
}

// --- Rollback da stato persistito (R4) ---------------------------------------

fn run_rollback(args: &RollbackArgs) -> Result<()> {
    let _log_guard = odoo_installer::logging::init(args.dry_run);

    let state_path = args
        .state
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_STATE_PATH));

    let state = InstallState::load(&state_path).map_err(|e| anyhow!(e))?;

    // Nessuno stato = niente da annullare. È la condizione normale su una
    // macchina pulita o dopo un rollback già completato: non è un errore.
    if state.completed.is_empty() {
        println!(
            "Nessuna installazione da annullare: {} non esiste o non registra alcuno step.",
            state_path.display()
        );
        return Ok(());
    }

    // Senza la configurazione persistita non si sa *quale* utente, *quale*
    // database, *quale* directory annullare — e indovinarli dai default
    // significherebbe rischiare di droppare un database che non abbiamo creato
    // noi. Meglio fermarsi ed elencare cosa c'è, così la pulizia manuale è
    // almeno informata.
    let Some(config) = state.config.clone() else {
        eprintln!(
            "Il file di stato {} è stato scritto da una versione precedente dell'installer e \n\
             non contiene la configurazione dell'installazione (utente, database, directory).\n\
             Senza quei dati un rollback automatico dovrebbe indovinarli, e indovinare qui \n\
             significa rischiare di rimuovere risorse che non abbiamo creato noi.\n\n\
             Step registrati da quell'installazione, in ordine di esecuzione:",
            state_path.display()
        );
        for record in &state.completed {
            eprintln!("  - {}", record.name);
        }
        bail!(
            "rollback automatico non disponibile per questo file di stato: ripulisci a mano \
             gli artefatti elencati, poi rimuovi {}",
            state_path.display()
        );
    };

    print_rollback_summary(&state, &state_path, &config, args);

    let interactive = prompt::is_interactive();

    // Conferma prima di un'operazione distruttiva. La politica è una funzione
    // pura (`confirmation_gate`) così è verificabile senza terminale.
    match rollback::confirmation_gate(args.dry_run, args.yes, interactive) {
        ConfirmationGate::Proceed => {}
        ConfirmationGate::Ask => {
            if !prompt::confirm("Procedere con la rimozione elencata sopra?")? {
                bail!("Rollback annullato dall'utente. Nessuna modifica effettuata.");
            }
        }
        ConfirmationGate::RefuseNonInteractive => bail!(
            "il rollback rimuove risorse dal sistema e richiede una conferma. \
             Senza terminale interattivo usa --yes per confermare esplicitamente."
        ),
    }

    // Root e lock solo per un rollback reale: il dry-run non tocca il sistema e
    // deve poter girare anche da utente normale.
    let _lock = if args.dry_run {
        None
    } else {
        checks::check_root().map_err(|e| anyhow!(e))?;
        Some(
            odoo_installer::lockfile::acquire(Path::new(
                odoo_installer::lockfile::DEFAULT_LOCK_PATH,
            ))
            .map_err(|e| anyhow!(e))?,
        )
    };

    let ctx = config.to_context(args.dry_run, args.aggressive_rollback, state_path.clone());

    let report = {
        let reporter: Box<dyn ProgressReporter> = if interactive && !args.dry_run {
            Box::new(IndicatifReporter::new(state.completed.len()))
        } else {
            Box::new(LogReporter)
        };
        rollback::rollback_from_state(&state, &ctx, &steps::real_ops, reporter.as_ref())
    };

    print_rollback_report(&report, args.dry_run);

    if args.dry_run {
        println!("dry-run: nessuna modifica applicata, il file di stato resta al suo posto.");
        return Ok(());
    }

    // Lo stato è consumato solo se il rollback è andato a fondo: se qualcosa è
    // rimasto, il file resta e una seconda esecuzione può riprovare (gli undo
    // sono idempotenti).
    if report.is_clean() {
        match InstallState::clear(&state_path) {
            Ok(()) => println!("Stato consumato: {} rimosso.", state_path.display()),
            Err(e) => tracing::warn!(
                path = %state_path.display(),
                error = %e,
                "rimozione del file di stato fallita: rimuovilo a mano"
            ),
        }
    } else {
        println!(
            "Il file di stato {} NON è stato rimosso: descrive ancora ciò che non è stato \n\
             ripulito. Sistemato il problema, puoi rieseguire `odoo-installer rollback` \n\
             (gli undo sono idempotenti).",
            state_path.display()
        );
    }

    Ok(())
}

/// Riepilogo pre-conferma: cosa verrà annullato e in quale ordine.
fn print_rollback_summary(
    state: &InstallState,
    state_path: &Path,
    config: &odoo_installer::state::InstallConfig,
    args: &RollbackArgs,
) {
    println!();
    println!("================================================================");
    match rollback::install_status(state) {
        InstallStatus::Complete { steps } => {
            println!("Installazione COMPLETA ({steps} step): verrà disinstallata.")
        }
        InstallStatus::Interrupted { done, total } => println!(
            "Installazione INTERROTTA a metà ({done} step su {total} completati):\n\
             verranno ripuliti i residui lasciati da quella esecuzione."
        ),
    }
    println!("Stato di partenza: {}", state_path.display());
    println!();
    println!("Artefatti dell'installazione:");
    println!("  Versione Odoo : {}", config.odoo_version);
    println!("  Utente Odoo   : {}", config.odoo_user);
    println!("  Database      : {}", config.db_name);
    println!("  DB user       : {}", config.db_user);
    println!("  Install dir   : {}", config.install_dir.display());
    println!(
        "  Nginx         : {}",
        if config.with_nginx {
            "configurato"
        } else {
            "no"
        }
    );
    println!();
    println!("Step da annullare, in ordine inverso di esecuzione:");
    for (i, name) in rollback::undo_plan(state).iter().enumerate() {
        println!("  {:>2}. {name}", i + 1);
    }
    println!();
    println!(
        "Verrà rimosso SOLO ciò che l'installer ha creato: risorse preesistenti\n\
         (database, pacchetti, config, utenti già presenti) restano intatte."
    );
    if !args.aggressive_rollback {
        println!(
            "PostgreSQL, Nginx e le utility comuni (git/curl/wget) restano installati:\n\
             usa --aggressive-rollback per purgare anche quelli."
        );
    }
    if args.dry_run {
        println!("Modalità      : dry-run (nessuna mutazione)");
    }
    println!("================================================================");
    println!();
}

/// Report di fine rollback: cosa è stato annullato e — soprattutto — cosa no.
fn print_rollback_report(report: &RollbackReport, dry_run: bool) {
    let verbo = if dry_run { "annullerebbe" } else { "annullati" };
    println!();
    println!("================================================================");
    println!(
        "Rollback: {} {} step su {}.",
        verbo,
        report.undone(),
        report.outcomes.len()
    );

    let residue = report.residue();
    if residue.is_empty() {
        println!("Nessun residuo: il sistema è tornato allo stato precedente.");
        println!("================================================================");
        println!();
        return;
    }

    println!();
    println!(
        "ATTENZIONE — {} step non ripuliti del tutto.",
        residue.len()
    );
    println!("Il rollback è best-effort: prosegue anche quando un undo fallisce, ma");
    println!("ciò che quell'undo non ha rimosso è ancora sul sistema. Da verificare");
    println!("e rimuovere a mano:");
    for item in residue {
        match &item.outcome {
            UndoOutcome::Failed(e) => println!("  - {}: undo fallito ({e})", item.name),
            UndoOutcome::Unknown => println!(
                "  - {}: step sconosciuto a questa versione dell'installer",
                item.name
            ),
            UndoOutcome::NotRehydrated(e) => println!(
                "  - {}: snapshot illeggibile, undo saltato per sicurezza ({e})",
                item.name
            ),
            UndoOutcome::Undone => {}
        }
    }
    println!();
    println!("Il dettaglio completo è nel log (/opt/odoo/.installer.log).");
    println!("================================================================");
    println!();
}
