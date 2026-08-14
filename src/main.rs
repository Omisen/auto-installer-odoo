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
            tracing::info!(config = %path.display(), ".env configuration loaded");
            raw
        }
        None => RawConfig::default(),
    };

    // prompts only for fields not passed on the CLI; empty without a TTY.
    let prompted = if interactive {
        prompt::collect(&cli_raw, &env_raw)?
    } else {
        tracing::info!(
            "no interactive input available: using the CLI, the .env and the final defaults"
        );
        RawConfig::default()
    };

    let resolved = ResolvedConfig::resolve(&cli_raw, &env_raw, &prompted, interactive)
        .map_err(|e| anyhow!(e))?;

    // the non-interactive hard stop already ran inside `resolve`.
    if let AdminConfirm::ConfirmNeeded =
        config::check_admin_password(resolved.admin_passwd.expose(), interactive)?
    {
        tracing::warn!(
            "the Odoo admin password is set to the weak value 'admin': use it only for demos or throwaway environments."
        );
        if !prompt::confirm("Confirm you want to continue with admin_passwd='admin'?")? {
            bail!("installation stopped: set an admin password other than 'admin'.");
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
            "this version of the installer has no backend for family '{}': it can neither \
             install nor remove packages on this system",
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
        println!("=== PLAN (dry run) — nothing on the system is changed ===");
        // the plan **interrogates** the system, and some snapshots go through
        // `sudo`. unprivileged, those steps are skipped, so the plan is true
        // but incomplete — better said upfront than found in a stray warning
        // (A-V3-11).
        if !checks::running_as_root() {
            println!(
                "note: without sudo some steps cannot inspect the system (PostgreSQL, \n\
                 installed packages) and will be listed as \"snapshot unavailable\".\n\
                 for the complete plan: sudo invok --dry-run …\n"
            );
        }
        dry_run_plan(&mut steps, &ctx, &LogReporter);
        println!("=== end of plan (dry run) ===");
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
        Start::Fresh => "Proceed with the installation?",
        Start::Resume(_) => "Resume the interrupted installation?",
        Start::Replace => "Reinstall from scratch, setting the existing manifest aside?",
    };
    if interactive && !prompt::confirm(question)? {
        bail!("installation cancelled by the user.");
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
                archived = %saved.display(),
                "--force: the previous manifest was set aside, not deleted"
            );
            println!(
                "--force: the previous manifest was archived in {}.\n\
                 if that installation had created artifacts, this is the only record of \
                 what to remove: keep it (`invok rollback --state <file>`).",
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
            "cannot mark the state as finished: the installation succeeded anyway, but \
             `invok rollback` may not be able to uninstall it"
        );
    }

    print_install_summary(&ctx);
    tracing::info!("preparation complete");
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
    let requested = InstallConfig::from_context(ctx);

    match state::start_decision(&state, &requested, force) {
        StartDecision::Fresh => Ok(Start::Fresh),
        StartDecision::Replace => Ok(Start::Replace),

        StartDecision::Resume => {
            println!(
                "interrupted installation found in {}: {} steps already completed, resuming.\n\
                 the steps already run are not repeated, and the ownership of the artifacts \n\
                 recorded back then is preserved.\n\
                 (to start over: `sudo invok rollback`, or `--force`.)",
                ctx.state_path.display(),
                state.completed.len()
            );
            Ok(Start::Resume(Box::new(state)))
        }

        StartDecision::RefuseFinished => {
            let instance = state
                .config
                .as_ref()
                .map(|c| {
                    format!(
                        "Odoo {}, user '{}', database '{}', in {}",
                        c.odoo_version,
                        c.odoo_user,
                        c.db_name,
                        c.install_dir.display()
                    )
                })
                .unwrap_or_else(|| "configuration not recorded".to_string());
            bail!(
                "a completed installation is already registered on this machine.\n\
                 \n  Manifest : {}\n  Instance : {}\n  Steps    : {} recorded\n\
                 \n\
                 to remove it:                              sudo invok rollback\n\
                 to reinstall over it (manifest set aside): --force\n\
                 \n\
                 proceeding without an explicit choice would overwrite the manifest with \
                 every artifact marked pre-existing, and this instance would no longer be \
                 removable automatically.",
                ctx.state_path.display(),
                instance,
                state.completed.len()
            )
        }

        StartDecision::RefuseIdentityMismatch(differences) => {
            let listing: Vec<String> = differences
                .iter()
                .map(|(field, before, now)| format!("  {field}: '{before}' -> '{now}'"))
                .collect();
            bail!(
                "there is an interrupted installation ({} steps) in {}, but the requested \
                 parameters name different artifacts:\n{}\n\
                 \n\
                 resuming like this would produce a manifest halfway between two instances, \
                 and the rollback would act partly on the wrong artifacts.\n\
                 \n\
                 re-run with the same parameters to resume, or \
                 `sudo invok rollback` to clean up first.",
                state.completed.len(),
                ctx.state_path.display(),
                listing.join("\n")
            )
        }

        StartDecision::RefuseUnknownIdentity => bail!(
            "there is an interrupted installation ({} steps) in {}, but the manifest does not \
             record the configuration (a format older than R4): it cannot be verified that it \
             describes the same artifacts, so it is not resumed.\n\
             \n\
             use `sudo invok rollback --state {}` to clean up, or `--force` \
             to start over with the manifest set aside.",
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
        .map_err(|e| anyhow!("cannot archive {}: {e}", path.display()))?;
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
            "the port is held by this installation's own service: check skipped (resume)"
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
        "the 'admin' default (allowed only with an explicit confirmation; discouraged)".to_string()
    } else {
        "custom".to_string()
    };

    println!();
    println!("================================================================");
    println!("Final installation settings:");
    println!("  Odoo version : {}", ctx.odoo_version);
    println!("  Odoo user    : {}", ctx.odoo_user);
    println!("  Database     : {}", ctx.db_name);
    println!("  DB user      : {}", ctx.db_user);
    println!("  HTTP port    : {}", ctx.port);
    println!("  Install dir  : {}", ctx.install_dir.display());
    println!(
        "  Nginx        : {}",
        if ctx.with_nginx { "enabled" } else { "no" }
    );
    println!("  Admin passwd : {admin_line}");
    if ctx.dry_run {
        println!("  Mode         : dry run (nothing is changed)");
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
    println!("Installation complete.");
    println!();
    println!(
        "  Odoo {}          http://localhost:{}",
        ctx.odoo_version, ctx.port
    );
    println!("  Service          {unit} (systemd)");
    println!("  System user      {}", ctx.odoo_user);
    println!("  Database         {} (role {})", ctx.db_name, ctx.db_user);
    println!("  Sources          {}", ctx.install_dir.display());
    println!(
        "  Config           {}/odoo{}.conf",
        ctx.install_dir.display(),
        ctx.odoo_version_short
    );
    if let Some(logfile) = &ctx.odoo_logfile {
        println!("  Odoo log         {}", logfile.display());
    } else {
        println!("  Odoo log         journal (journalctl -u {unit})");
    }
    if ctx.with_nginx {
        println!("  Nginx            reverse proxy serving on :80");
    }
    println!();
    println!("  Management       odoo start|stop|restart|status   (reopen the shell)");
    println!("  Uninstall        sudo invok rollback");
    println!();
    println!(
        "the state in {} is the uninstall manifest: it says what was\n\
         created and what was already there. do not remove it, the rollback needs it.",
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
        "note: you can interrupt with Ctrl-C. the installer **undoes its own work** and \n\
         the system comes back as it was.\n\
     \n\
         the interruption takes effect between one step and the next: the step in progress \n\
         is carried to completion, because stopping an `apt` or a database initialisation \n\
         halfway would leave something worse than what was being avoided.\n\
     \n\
         a second Ctrl-C exits **at once**: the system then stays half-done and is cleaned \n\
         up with\n\
     \n    sudo invok rollback\n\n\
         the state needed for that is recorded in {} after every step — it holds even if \n\
         the machine loses power.\n",
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
            "nothing to undo: {} does not exist or records no step.",
            state_path.display()
        );
        return Ok(());
    }

    // without the persisted config we do not know *which* user, database or
    // directory to undo, and guessing from the defaults risks dropping a
    // database we never created. better to stop and list what is there.
    let Some(config) = state.config.clone() else {
        eprintln!(
            "the state file {} was written by an older version of the installer and does \n\
             not carry the installation's configuration (user, database, directory).\n\
             without those an automatic rollback would have to guess, and guessing here \n\
             means risking the removal of resources we never created.\n\n\
             steps recorded by that installation, in order of execution:",
            state_path.display()
        );
        for record in &state.completed {
            eprintln!("  - {}", record.name);
        }
        bail!(
            "no automatic rollback is available for this state file: clean up the listed \
             artifacts by hand, then remove {}",
            state_path.display()
        );
    };

    // written by a different installer? (A-V3-16) not a refusal, but the
    // context the "unknown step" warning lacked — and said BEFORE starting, so
    // the reader can stop and use the right version.
    if let Some(note) = state::version_mismatch_note(
        config.installer_version.as_deref(),
        invok::INSTALLER_VERSION,
    ) {
        tracing::warn!("{note}");
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
    tracing::info!(family = %config.os_family, "rollback: family read from the manifest");
    let detected = checks::os_id_from(Path::new(checks::OS_RELEASE_PATH))
        .and_then(|id| invok::distro::OsFamily::from_os_id(&id));
    if let Some(warning) = invok::distro::family_mismatch(config.os_family, detected) {
        tracing::warn!("{warning}");
        eprintln!("Warning: {warning}");
    }

    // backends chosen by the **manifest's** family, not this machine's. with
    // none available we stop here: removing packages with the wrong manager
    // removes nothing and reports success.
    let make_ops = invok::system_ops::backend_factory(config.os_family).ok_or_else(|| {
        anyhow!(
            "the manifest describes an installation on '{}', but this version of the installer \
             has no backend for that family and cannot undo it. use a binary that supports it.",
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
            if !prompt::confirm("Proceed with the removal listed above?")? {
                bail!("rollback cancelled by the user. nothing was changed.");
            }
        }
        ConfirmationGate::RefuseNonInteractive => bail!(
            "the rollback removes resources from the system and needs a confirmation. \
             without an interactive terminal, use --yes to confirm explicitly."
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
        println!("dry run: nothing was changed, the state file stays where it is.");
        return Ok(());
    }

    // the state is consumed only if the rollback went all the way: anything
    // left keeps the file, so a second run can retry.
    if report.is_clean() {
        match InstallState::clear(&state_path) {
            Ok(()) => println!("State consumed: {} removed.", state_path.display()),
            Err(e) => tracing::warn!(
                path = %state_path.display(),
                error = %e,
                "removing the state file failed: remove it by hand"
            ),
        }
    } else {
        println!(
            "the state file {} was NOT removed: it still describes what was not cleaned \n\
             up. once the problem is fixed you can run `invok rollback` again \n\
             (the undos are idempotent).",
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
            println!("COMPLETE installation ({steps} steps): it will be uninstalled.")
        }
        InstallStatus::Interrupted { done, total } => println!(
            "INTERRUPTED installation ({done} of {total} steps completed):\n\
             the leftovers of that run will be cleaned up."
        ),
    }
    println!("Starting state: {}", state_path.display());
    println!();
    println!("Artifacts of the installation:");
    println!("  Odoo version : {}", config.odoo_version);
    println!("  Odoo user    : {}", config.odoo_user);
    println!("  Database     : {}", config.db_name);
    println!("  DB user      : {}", config.db_user);
    println!("  Install dir  : {}", config.install_dir.display());
    println!(
        "  Nginx        : {}",
        if config.with_nginx {
            "configured"
        } else {
            "no"
        }
    );
    println!();
    println!("Steps to undo, in reverse order of execution:");
    for (i, name) in rollback::undo_plan(state).iter().enumerate() {
        println!("  {:>2}. {name}", i + 1);
    }
    println!();
    println!(
        "ONLY what the installer created will be removed: pre-existing resources\n\
         (databases, packages, configs, users already there) stay untouched."
    );
    if !args.aggressive_rollback {
        println!(
            "PostgreSQL, nginx and the common utilities (git/curl/wget) stay installed:\n\
             use --aggressive-rollback to purge those too."
        );
    }
    if args.dry_run {
        println!("Mode          : dry run (nothing is changed)");
    }
    println!("================================================================");
    println!();
}

/// end-of-rollback report: what was undone and, above all, what was not.
fn print_rollback_report(report: &RollbackReport, dry_run: bool) {
    let verb = if dry_run { "would undo" } else { "undone" };
    println!();
    println!("================================================================");
    println!(
        "Rollback: {} {} of {} steps.",
        verb,
        report.undone(),
        report.outcomes.len()
    );

    let residue = report.residue();
    if residue.is_empty() {
        match &report.home_left_behind {
            // the promise, not the mechanism (A-MD-2): every undo can have
            // succeeded with the home still there, because `PrepareOptRoot`
            // correctly gives up on a non-empty directory and returns `Ok`.
            None => println!("No leftovers: the system is back to its previous state."),
            Some(home) => {
                println!("Every undo succeeded, but {} still exists.", home.display());
                println!();
                println!("It was not removed because it is **not empty**: it holds something");
                println!("we did not create, and we never `rm -rf` other people's things.");
                println!("Look at what is inside and decide for yourself:");
                println!();
                println!("    sudo ls -la {}", home.display());
                println!();
                println!("Everything the installer had created has been removed.");
            }
        }
        println!("================================================================");
        println!();
        return;
    }

    println!();
    println!(
        "WARNING — {} steps were not fully cleaned up.",
        residue.len()
    );
    println!("The rollback is best-effort: it carries on when an undo fails, but");
    println!("what that undo did not remove is still on the system. To check and");
    println!("remove by hand:");
    for item in residue {
        match &item.outcome {
            UndoOutcome::Failed(e) => println!("  - {}: undo failed ({e})", item.name),
            UndoOutcome::Unknown => println!(
                "  - {}: step unknown to this version of the installer",
                item.name
            ),
            UndoOutcome::NotRehydrated(e) => println!(
                "  - {}: snapshot unreadable, undo skipped for safety ({e})",
                item.name
            ),
            UndoOutcome::Undone => {}
        }
    }
    println!();
    println!(
        "The full detail is in the log ({}).",
        invok::logging::DEFAULT_LOG_PATH
    );
    println!("================================================================");
    println!();
}
