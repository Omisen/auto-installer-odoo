//! entry point: dispatch between the installer's two commands.
//!
//! - **no subcommand** → install: parse the CLI, load the `.env`, prompt when
//!   there is a TTY, resolve the cascade, build the [`Context`], run the
//!   preflight checks, then execute the steps with automatic rollback on
//!   error.
//! - **`rollback`** (alias `uninstall`) → undo an installation from the
//!   persisted state. see [`invok::rollback`].

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};
use clap::Parser;

use invok::checks;
use invok::cli::{Cli, Command, RollbackArgs};
use invok::config::{self, AdminConfirm, RawConfig, ResolvedConfig};
use invok::context::Context;
use invok::engine::{dry_run_plan, Installer};
use invok::progress::{IndicatifReporter, LogReporter, ProgressReporter};
use invok::prompt;
use invok::rollback::{self, ConfirmationGate, InstallStatus, RollbackReport, UndoOutcome};
use invok::state::{self, InstallConfig, InstallState, StartDecision};
use invok::steps;

fn main() -> Result<()> {
    let cli = Cli::parse();
    match &cli.command {
        Some(Command::Rollback(args)) => run_rollback(args),
        // no subcommand: install, as it has always been.
        None => run_install(&cli),
    }
}

// --- installation -----------------------------------------------------------

fn run_install(cli: &Cli) -> Result<()> {
    // the guard must stay alive for the whole run.
    let _log_guard = invok::logging::init(cli.dry_run);

    let interactive = prompt::is_interactive();

    // raw sources: the CLI and the optional `.env`, parsed and never run.
    let cli_raw = RawConfig::from_cli(cli);
    let env_raw = match &cli.config {
        Some(path) => {
            let raw = config::parse_env_file(path).map_err(|e| anyhow!(e))?;
            tracing::info!(config = %path.display(), "configurazione .env caricata");
            raw
        }
        None => RawConfig::default(),
    };

    // prompts only for fields not passed on the CLI; empty without a TTY.
    let prompted = if interactive {
        prompt::collect(&cli_raw, &env_raw)?
    } else {
        tracing::info!("input interattivo non disponibile: uso CLI, .env e default finali");
        RawConfig::default()
    };

    let resolved = ResolvedConfig::resolve(&cli_raw, &env_raw, &prompted, interactive)
        .map_err(|e| anyhow!(e))?;

    // the non-interactive hard stop already ran inside `resolve`.
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

    // work on the manifest we **find**: the current path, or a historical one
    // when that is the only one there.
    let state_path = invok::state::resolve_state_path();
    let mut ctx = Context::from_resolved(resolved, cli.dry_run, state_path);
    ctx.aggressive_rollback = cli.aggressive_rollback;
    ctx.sudo_user = std::env::var("SUDO_USER").ok().filter(|s| !s.is_empty());

    print_configuration(&ctx);

    // the OS is read **first**: the family comes from here, and the backends
    // the steps are built with come from the family. a pure read that needs no
    // root, so it can precede the `--dry-run` branch, which needs the family
    // too.
    //
    // it does not sit between A-R9-1's three preflights: it is above all of
    // them, depends on no manifest, and its error is unmistakable.
    let os_info = checks::check_os().map_err(|e| anyhow!(e))?;
    ctx.os_family = os_info.family;

    // the backends for this family. `None` is handled **once**, here: from then
    // on the factory cannot fail, which is what lets the steps be built without
    // any of them knowing which distribution they run on.
    let make_ops = invok::system_ops::backend_factory(ctx.os_family).ok_or_else(|| {
        anyhow!(
            "questa versione dell'installer non ha un backend per la famiglia '{}': \
             non posso installare né rimuovere pacchetti su questo sistema",
            ctx.os_family
        )
    })?;

    // the venv's interpreter (M11). the order matters: after `check_os`, which
    // supplies the family, and before any step, because two of them must read
    // the same answer. a query, not a mutation.
    ctx.python = checks::plan_python(make_ops().as_ref());

    let mut steps = steps::build_steps(&make_ops);

    // --- dry-run: print the plan, mutate nothing, persist nothing -----------
    if ctx.dry_run {
        println!("=== PIANO (dry-run) — nessuna modifica al sistema ===");
        // the plan **interrogates** the system, and some snapshots go through
        // `sudo`. unprivileged, those steps are skipped, so the plan is true
        // but incomplete — better said upfront than found in a stray warning
        // (A-V3-11).
        if !checks::running_as_root() {
            println!(
                "Nota: senza sudo alcuni step non possono ispezionare il sistema (PostgreSQL, \n\
                 pacchetti installati) e verranno elencati come «snapshot non disponibile».\n\
                 Per il piano completo: sudo invok --dry-run …\n"
            );
        }
        dry_run_plan(&mut steps, &ctx, &LogReporter);
        println!("=== fine piano (dry-run) ===");
        return Ok(());
    }

    // the preflights run in TWO groups, and the order between them is not
    // cosmetic (A-R9-1, found by the integration CI).
    //
    // first **who** is running: root and sudo. needed at once, because the
    // manifest is `0600 root` and without this an unprivileged user would read
    // "permission denied" on a file they know nothing about.
    checks::check_caller().map_err(|e| anyhow!(e))?;

    // then **whether** this run should happen at all: fresh install, resume or
    // refusal (A-V3-1). a disk read only.
    //
    // **before** the environment checks, and the CI showed why: reinstalling
    // over a working instance means Odoo is holding the port, so `check_ports`
    // failed first and this check was never reached — in the very scenario it
    // exists for. the user was told to free the port, i.e. to stop Odoo,
    // instead of to use `rollback` or `--force`. a busy port is a
    // **consequence** of the existing installation, not its cause.
    let start = decide_start(&ctx, cli.force)?;

    // finally **whether the machine can host it**: OS, disk, ports, commands.
    //
    // the port check is skipped when we are the ones holding it: in a resume
    // the manifest says whether `setup-systemd` had run, and without that
    // exception an installation interrupted after it could never be resumed.
    let port_is_ours = matches!(&start, Start::Resume(state) if state.owns_the_http_port());
    run_environment_checks(&ctx, port_is_ours, &make_ops)?;
    ctx.os_info = Some(os_info);

    // final interactive confirmation before mutating anything.
    let question = match &start {
        Start::Fresh => "Procedere con l'installazione?",
        Start::Resume(_) => "Riprendere l'installazione interrotta?",
        Start::Replace => "Reinstallare da capo, mettendo da parte il manifesto esistente?",
    };
    if interactive && !prompt::confirm(question)? {
        bail!("Installazione annullata dall'utente.");
    }

    print_interrupt_notice(&ctx.state_path);

    // from here a Ctrl-C raises a flag the engine watches between steps, and
    // the installation undoes itself (B-V3-5). registered after the
    // confirmation and before the mutations: earlier there is nothing to undo,
    // and immediate exit is what the user expects.
    let interrupted = invok::interrupt::install();

    // exclusive lock against concurrent installations, released on `Drop`.
    // taken after the checks and before any mutation.
    let _lock = invok::lockfile::acquire(Path::new(invok::lockfile::DEFAULT_LOCK_PATH))
        .map_err(|e| anyhow!(e))?;

    // built **here**, not earlier: the bar's ticker redraws on stderr, the same
    // stream `inquire` uses, so a live bar during a prompt erases the user's
    // line. progress starts once there is progress to show.
    let reporter: Box<dyn ProgressReporter> = if interactive {
        Box::new(IndicatifReporter::new(steps.len()))
    } else {
        Box::new(LogReporter)
    };

    // `--force` moves the previous manifest aside, never deletes it. here
    // because it is a mutation, and those come after the confirm and the lock.
    let mut installer = match start {
        Start::Fresh => Installer::new(),
        Start::Resume(state) => Installer::resuming_from(*state),
        Start::Replace => {
            let saved = archive_manifest(&ctx.state_path)?;
            tracing::warn!(
                archiviato = %saved.display(),
                "--force: manifesto precedente messo da parte, non cancellato"
            );
            println!(
                "--force: manifesto precedente archiviato in {}.\n\
                 Se quell'installazione aveva creato artefatti, resta l'unica traccia di \
                 cosa rimuovere: conservalo (`invok rollback --state <file>`).",
                saved.display()
            );
            Installer::new()
        }
    }
    .watching_interrupt(interrupted);
    installer
        .execute_with_reporter(&mut steps, &ctx, reporter.as_ref())
        .map_err(|e| {
            // the in-process rollback already ran inside `execute`.
            anyhow!(e)
        })?;

    // on success the state is marked finished and **stays on disk**: it is the
    // uninstall manifest, without which this instance could not be removed
    // later (A-R5-1). failing here does not undo a successful installation, so
    // we report and carry on.
    if let Err(e) = installer.mark_finished(&ctx) {
        tracing::warn!(
            path = %ctx.state_path.display(),
            error = %e,
            "impossibile marcare lo stato come concluso: l'installazione è comunque \
             riuscita, ma `invok rollback` potrebbe non poterla disinstallare"
        );
    }

    print_install_summary(&ctx);
    tracing::info!("preparazione completata");
    Ok(())
}

/// where this run starts from.
///
/// a **decision**, not an action: computed by reading the disk and nothing
/// else, so it can be taken before the confirmation and before the lock.
enum Start {
    /// no usable manifest: first installation.
    Fresh,
    /// compatible partial manifest: resume where it stopped.
    ///
    /// `Box`ed because [`InstallState`] carries the whole step list, which
    /// would otherwise size every variant of the enum.
    Resume(Box<InstallState>),
    /// `--force` over an existing manifest: archive it and start over.
    Replace,
}

/// applies the start policy (A-V3-1) and formats its outcome for the user.
///
/// the **rule** lives in [`invok::state::start_decision`], pure and checkable
/// without a filesystem. what stays here is what is genuinely `main`'s: reading
/// the manifest and turning a refusal into an actionable message. A-V3-1 was
/// born from a decision that lived in `main`, where no test reaches.
///
/// # errors
///
/// a refusal, carrying the message to show.
fn decide_start(ctx: &Context, force: bool) -> Result<Start> {
    let state = InstallState::load(&ctx.state_path).map_err(|e| anyhow!(e))?;
    let richiesta = InstallConfig::from_context(ctx);

    match state::start_decision(&state, &richiesta, force) {
        StartDecision::Fresh => Ok(Start::Fresh),
        StartDecision::Replace => Ok(Start::Replace),

        StartDecision::Resume => {
            println!(
                "Installazione interrotta trovata in {}: {} step già completati, si riprende.\n\
                 Gli step già eseguiti non vengono rifatti e la proprietà degli artefatti \n\
                 registrata allora viene conservata.\n\
                 (Per ricominciare da capo: `sudo invok rollback`, oppure `--force`.)",
                ctx.state_path.display(),
                state.completed.len()
            );
            Ok(Start::Resume(Box::new(state)))
        }

        StartDecision::RefuseFinished => {
            let istanza = state
                .config
                .as_ref()
                .map(|c| {
                    format!(
                        "Odoo {}, utente '{}', database '{}', in {}",
                        c.odoo_version,
                        c.odoo_user,
                        c.db_name,
                        c.install_dir.display()
                    )
                })
                .unwrap_or_else(|| "configurazione non registrata".to_string());
            bail!(
                "Risulta già un'installazione completata su questa macchina.\n\
                 \n  Manifesto : {}\n  Istanza   : {}\n  Step      : {} registrati\n\
                 \n\
                 Per rimuoverla:                        sudo invok rollback\n\
                 Per reinstallare sopra (manifesto messo da parte):  --force\n\
                 \n\
                 Proseguire senza una scelta esplicita sovrascriverebbe il manifesto con \
                 artefatti tutti marcati come preesistenti, e questa istanza non sarebbe \
                 più disinstallabile automaticamente.",
                ctx.state_path.display(),
                istanza,
                state.completed.len()
            )
        }

        StartDecision::RefuseIdentityMismatch(differenze) => {
            let elenco: Vec<String> = differenze
                .iter()
                .map(|(campo, prima, ora)| format!("  {campo}: '{prima}' -> '{ora}'"))
                .collect();
            bail!(
                "C'è un'installazione interrotta ({} step) in {}, ma i parametri richiesti \
                 nominano artefatti diversi:\n{}\n\
                 \n\
                 Riprendere così produrrebbe un manifesto a metà fra due istanze, e il \
                 rollback agirebbe in parte sugli artefatti sbagliati.\n\
                 \n\
                 Rilancia con gli stessi parametri per riprendere, oppure \
                 `sudo invok rollback` per ripulire prima.",
                state.completed.len(),
                ctx.state_path.display(),
                elenco.join("\n")
            )
        }

        StartDecision::RefuseUnknownIdentity => bail!(
            "C'è un'installazione interrotta ({} step) in {}, ma il manifesto non registra \
             la configurazione (formato precedente alla R4): non posso verificare che \
             descriva gli stessi artefatti, quindi non la riprendo.\n\
             \n\
             Usa `sudo invok rollback --state {}` per ripulire, oppure `--force` \
             per ricominciare mettendo da parte il manifesto.",
            state.completed.len(),
            ctx.state_path.display(),
            ctx.state_path.display()
        ),
    }
}

/// moves the manifest aside rather than letting it be overwritten (`--force`).
///
/// never deleted: if that installation created artifacts, this file is the
/// **only** record of which. the name carries a timestamp so it cannot
/// overwrite an earlier archive.
///
/// # errors
///
/// propagates the rename failure.
fn archive_manifest(path: &Path) -> Result<PathBuf> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".superseded-{stamp}"));
    let target = path.with_file_name(name);
    std::fs::rename(path, &target)
        .map_err(|e| anyhow!("impossibile archiviare {}: {e}", path.display()))?;
    Ok(target)
}

/// **environment** checks: can this machine host the installation?
///
/// they mutate nothing and fail before any mutation. separate from the caller
/// checks ([`checks::check_caller`]) because they answer a different question
/// at a different moment (A-R9-1).
///
/// `port_is_ours` skips the port check: in a resume the listening service is
/// ours, and refusing over a conflict with ourselves would make the very
/// installation being resumed unresumable.
///
/// # errors
///
/// the first failing check.
fn run_environment_checks(
    ctx: &Context,
    port_is_ours: bool,
    make_ops: &dyn Fn() -> Box<dyn invok::system_ops::SystemOps>,
) -> Result<()> {
    let required_gb = std::env::var("MIN_DISK_GB")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(checks::DEFAULT_MIN_DISK_GB);
    checks::check_disk(&ctx.odoo_home, required_gb).map_err(|e| anyhow!(e))?;

    if port_is_ours {
        tracing::info!(
            port = ctx.port,
            "porta occupata dal servizio della stessa installazione: controllo saltato (resume)"
        );
    } else {
        // if nginx is already serving, port 80 is its own: not a conflict but
        // the program we are about to configure (A-V3-15).
        let nginx_already_serving = ctx.with_nginx && make_ops().service_is_active("nginx");
        checks::check_ports(ctx.port, ctx.with_nginx, nginx_already_serving)
            .map_err(|e| anyhow!(e))?;
    }
    checks::check_commands(ctx.os_family).map_err(|e| anyhow!(e))?;

    Ok(())
}

/// prints the resolved configuration. never prints the password.
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

/// end-of-installation summary: what was created and how to touch it (B2).
///
/// the rollback had its report since R4, while a successful installation ended
/// with a single log line.
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
    println!("  Disinstallazione sudo invok rollback");
    println!();
    println!(
        "Lo stato in {} è il manifesto di disinstallazione: dice cosa\n\
         è stato creato e cosa era già presente. Non rimuoverlo, serve al rollback.",
        ctx.state_path.display()
    );
    println!("================================================================");
    println!();
}

/// tells the user, up front, what happens if the run is interrupted.
///
/// since R18 a Ctrl-C rolls back on its own, and a **second** one exits at once
/// leaving the system half-done. that is worth knowing *before* pressing it,
/// not after.
fn print_interrupt_notice(state_path: &Path) {
    println!(
        "Nota: puoi interrompere con Ctrl-C. L'installer **annulla da sé** quello che ha \n\
         già fatto e il sistema torna come prima.\n\
     \n\
         L'interruzione ha effetto fra uno step e il successivo: lo step in corso viene \n\
         portato a termine, perché fermare a metà un `apt` o l'inizializzazione di un \n\
         database lascerebbe qualcosa di peggio di ciò che si voleva evitare.\n\
     \n\
         Un secondo Ctrl-C esce **subito**: in quel caso il sistema resta a metà e si \n\
         ripulisce con\n\
     \n    sudo invok rollback\n\n\
         Lo stato necessario è registrato in {} dopo ogni step — vale anche se la \n\
         macchina si spegne.\n",
        state_path.display()
    );
}

// --- rollback from persisted state (R4) -------------------------------------

fn run_rollback(args: &RollbackArgs) -> Result<()> {
    let _log_guard = invok::logging::init(args.dry_run);

    // without `--state`, look where we write today and then where older
    // versions did: an older instance must stay uninstallable.
    let state_path = args
        .state
        .clone()
        .unwrap_or_else(invok::state::resolve_state_path);

    let state = InstallState::load(&state_path).map_err(|e| anyhow!(e))?;

    // no state means nothing to undo: the normal condition on a clean machine
    // or after a completed rollback, not an error.
    if state.completed.is_empty() {
        println!(
            "Nessuna installazione da annullare: {} non esiste o non registra alcuno step.",
            state_path.display()
        );
        return Ok(());
    }

    // without the persisted config we do not know *which* user, database or
    // directory to undo, and guessing from the defaults risks dropping a
    // database we never created. better to stop and list what is there.
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

    // written by a different installer? (A-V3-16) not a refusal, but the
    // context the "unknown step" warning lacked — and said BEFORE starting, so
    // the reader can stop and use the right version.
    if let Some(nota) = state::version_mismatch_note(
        config.installer_version.as_deref(),
        invok::INSTALLER_VERSION,
    ) {
        tracing::warn!("{nota}");
    }

    // does the manifest describe an installation of THIS installer? (A-V3-8)
    // every path that will drive an `rm -rf`, a `dropdb` or a `userdel` comes
    // from this file, and `--state` accepts any path. the only anchor is a
    // value that does NOT come from it: `ODOO_HOME`.
    config.validate_perimeter().map_err(|e| anyhow!(e))?;

    // and is the file itself trustworthy? only for a real rollback: a dry run
    // merely prints, and inspecting a copied manifest is useful.
    if !args.dry_run {
        state::ensure_trustworthy(&state_path).map_err(|e| anyhow!(e))?;
    }

    // the family is READ from the manifest and used: it says which commands
    // created those artifacts. the system is read only to WARN, never to
    // decide, because a pre-2.3 manifest falls back to `Debian` and `--state`
    // accepts another machine's manifest. logging it is not decorative: a
    // default nobody sees is a default nobody can contradict.
    tracing::info!(famiglia = %config.os_family, "rollback: famiglia letta dal manifesto");
    let detected = checks::os_id_from(Path::new(checks::OS_RELEASE_PATH))
        .and_then(|id| invok::distro::OsFamily::from_os_id(&id));
    if let Some(avviso) = invok::distro::family_mismatch(config.os_family, detected) {
        tracing::warn!("{avviso}");
        eprintln!("Attenzione: {avviso}");
    }

    // backends chosen by the **manifest's** family, not this machine's. with
    // none available we stop here: removing packages with the wrong manager
    // removes nothing and reports success.
    let make_ops = invok::system_ops::backend_factory(config.os_family).ok_or_else(|| {
        anyhow!(
            "il manifesto descrive un'installazione su '{}', ma questa versione dell'installer \
             non ha un backend per quella famiglia: non posso annullarla. Usa un binario che la \
             supporti.",
            config.os_family
        )
    })?;

    print_rollback_summary(
        &state,
        &state_path,
        &config,
        args,
        steps::canonical_step_names(&make_ops).len(),
    );

    let interactive = prompt::is_interactive();

    // the policy is a pure function, checkable without a terminal.
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

    // root and lock only for a real rollback: a dry run touches nothing and
    // must run unprivileged.
    let _lock = if args.dry_run {
        None
    } else {
        checks::check_root().map_err(|e| anyhow!(e))?;
        Some(
            invok::lockfile::acquire(Path::new(invok::lockfile::DEFAULT_LOCK_PATH))
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
        rollback::rollback_from_state(&state, &ctx, &make_ops, reporter.as_ref())
    };

    print_rollback_report(&report, args.dry_run);

    if args.dry_run {
        println!("dry-run: nessuna modifica applicata, il file di stato resta al suo posto.");
        return Ok(());
    }

    // the state is consumed only if the rollback went all the way: anything
    // left keeps the file, so a second run can retry.
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
             ripulito. Sistemato il problema, puoi rieseguire `invok rollback` \n\
             (gli undo sono idempotenti).",
            state_path.display()
        );
    }

    Ok(())
}

/// pre-confirmation summary: what will be undone, and in which order.
fn print_rollback_summary(
    state: &InstallState,
    state_path: &Path,
    config: &invok::state::InstallConfig,
    args: &RollbackArgs,
    canonical_steps: usize,
) {
    println!();
    println!("================================================================");
    match rollback::install_status(state, canonical_steps) {
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

/// end-of-rollback report: what was undone and, above all, what was not.
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
        match &report.home_left_behind {
            // the promise, not the mechanism (A-MD-2): every undo can have
            // succeeded with the home still there, because `PrepareOptRoot`
            // correctly gives up on a non-empty directory and returns `Ok`.
            None => println!("Nessun residuo: il sistema è tornato allo stato precedente."),
            Some(home) => {
                println!(
                    "Tutti gli undo sono riusciti, ma {} esiste ancora.",
                    home.display()
                );
                println!();
                println!("Non l'abbiamo rimossa perché **non è vuota**: contiene qualcosa che");
                println!("non abbiamo creato noi, e su roba altrui non facciamo mai `rm -rf`.");
                println!("Guarda cosa c'è dentro e decidi tu:");
                println!();
                println!("    sudo ls -la {}", home.display());
                println!();
                println!("Tutto ciò che l'installer aveva creato è stato rimosso.");
            }
        }
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
    println!(
        "Il dettaglio completo è nel log ({}).",
        invok::logging::DEFAULT_LOG_PATH
    );
    println!("================================================================");
    println!();
}
