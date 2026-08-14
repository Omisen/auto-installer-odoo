//! the installer's steps, one per file.
//!
//! also the two entry points to the sequence: [`build_steps`], the canonical
//! installation order, and [`step_by_name`], which rebuilds one step from its
//! name for the rollback from disk. the two must cover the same set, and a test
//! verifies it.

use tracing::{info, warn};

use crate::packaging::PackageManager;
use crate::step::Step;
use crate::system_ops::{Downloader, RealDownloader, SystemOps};

/// removes packages in an undo, **repairing the manager** when it is
/// inconsistent. best-effort: it never fails, and logs what is left.
///
/// A-RT-2, found in the field: a rollback always runs *after* a failure, and
/// that failure may have left `dpkg` halfway. apt then refuses to operate and
/// **every** purge fails, leaving the packages we installed behind — the
/// surgical promise broken in the very scenario it matters most. on the test
/// VM, 24 packages stayed.
///
/// so: repair before removing (a harmless no-op on a healthy manager), and if
/// the removal still fails, a second attempt after the deep repair. failing
/// that we carry on, listing **exactly** what remains for the user to remove.
///
/// this is **policy**, not a command, which is why it lives here and not behind
/// [`PackageManager`]: that boundary stays 1:1 onto commands so tests can
/// assert exact sequences.
pub fn remove_with_recovery(pm: &dyn PackageManager, step: &str, pkgs: &[&str]) {
    if pkgs.is_empty() {
        return;
    }

    // pre-emptive recovery: apt does not operate on a broken dpkg.
    if let Err(e) = pm.try_repair() {
        warn!(step, error = %e, "undo: riparazione preventiva fallita, tento comunque la rimozione");
    }

    let Err(first) = pm.remove(pkgs) else {
        return;
    };
    warn!(step, error = %first, "undo: rimozione fallita, tento il recovery del gestore e riprovo");

    // covers unpacked-but-unconfigured packages, where the first repair alone
    // is not enough.
    if let Err(e) = pm.try_deep_repair() {
        warn!(step, error = %e, "undo: riparazione profonda fallita");
    }
    if let Err(e) = pm.try_repair() {
        warn!(step, error = %e, "undo: riparazione di recovery fallita");
    }

    match pm.remove(pkgs) {
        Ok(()) => info!(
            step,
            "undo: rimozione riuscita dopo il recovery del gestore"
        ),
        Err(second) => warn!(
            step,
            error = %second,
            residui = ?pkgs,
            "undo: rimozione fallita anche dopo il recovery del gestore, proseguo (best-effort). \
             Questi pacchetti restano installati: rimuovili a mano dopo aver sistemato il gestore \
             (`sudo apt-get install -f`)"
        ),
    }
}

/// the first **missing** level going down from `home` towards `target`: the
/// root of what a `mkdir -p` will create, and so the only thing an undo can
/// remove without touching somebody else's things.
///
/// `None` when `target` is not under `home`, or when everything already exists.
///
/// shared by the filestore and the cache steps, which are two cases of one
/// problem: creating a subdirectory inside the `odoo` user's home — which is
/// `Preexisting` and must not be emptied — while knowing *exactly* how much of
/// that tree we added.
pub fn highest_missing_level(
    ops: &dyn SystemOps,
    home: &std::path::Path,
    target: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let relative = target.strip_prefix(home).ok()?;
    let mut current = home.to_path_buf();
    for component in relative.components() {
        current = current.join(component);
        if !ops.path_exists(&current) {
            return Some(current);
        }
    }
    None
}

/// removes `created_root` recursively, with the **perimeter safety net**.
///
/// these undos delete a tree rooted at a path that comes **from disk**. a
/// corrupted state, or one written by another installation, must not become an
/// `rm -rf` elsewhere: the target must be a *strict* descendant of `home`,
/// otherwise it is logged and nothing is touched.
///
/// best-effort like every undo: a failure is a `warn!`, not an error that stops
/// the other steps' cleanup.
pub fn remove_created_root(
    ops: &dyn SystemOps,
    step: &str,
    home: &std::path::Path,
    target: &std::path::Path,
    dry_run: bool,
) {
    if !target.starts_with(home) || target == home {
        warn!(
            step,
            target = %target.display(),
            home = %home.display(),
            "undo: path fuori dal perimetro della home, non rimuovo nulla"
        );
        return;
    }
    if dry_run {
        info!(step, target = %target.display(), "undo (dry-run): rm -rf");
        return;
    }
    match ops.remove_dir_all(target) {
        Ok(()) => info!(step, target = %target.display(), "undo: rimosso"),
        Err(e) => warn!(
            step,
            target = %target.display(),
            error = %e,
            "undo: rm -rf fallito, proseguo (best-effort)"
        ),
    }
}

/// seconds since the epoch, for backup file names.
///
/// shared by four steps: the `<file>.bak.<epoch>` convention is one thing, and
/// four copies were four chances to diverge.
pub fn unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// a [`SystemOps`] factory: every step gets its own instance.
///
/// an `Fn` and not one `Box<dyn SystemOps>` because steps own their `ops`: N
/// instances are needed, not N references. in tests it hands out handles to one
/// `SystemModel`, so one step's mutations are visible to another's undo.
pub type OpsFactory<'a> = &'a dyn Fn() -> Box<dyn SystemOps>;

/// builds the production sequence, **in execution order**. the rollback undoes
/// it in reverse (invariant 2).
///
/// the canonical definition lives here and not in `main`: [`step_by_name`] must
/// cover it entirely for the rollback from disk to work, and a parity test
/// checks that — so adding a step in one place only breaks the test build, not
/// a rollback on a customer's machine.
///
/// it takes the `ops` factory because that is how the distribution family
/// enters the program **once**, from `main`, instead of being decided inside
/// twenty-two constructors. a blind `Step::new()` handing apt to everyone would
/// be the silent default multi-distro support exists to avoid.
///
/// the sequence is **one** for every family: a family changes *what a step
/// does*, never *which steps exist*. an inert step keeps its `PreState`
/// `Untracked`, which is the truth, while per-family sequences would mean
/// manifests of different shapes and a rollback that had to guess which.
///
/// **step names are never renamed**: they are the key that rebuilds the steps
/// of installations already in the field.
pub fn build_steps(make_ops: OpsFactory<'_>) -> Vec<Box<dyn Step>> {
    vec![
        Box::new(prepare_opt_root::PrepareOptRoot::with_ops(make_ops())),
        Box::new(create_odoo_user::CreateOdooUser::with_ops(make_ops())),
        Box::new(setup_log_dir::SetupLogDir::with_ops(make_ops())),
        // early on purpose: the snapshot must see the home BEFORE anything
        // running as `odoo` writes a cache there. early here means late in the
        // undo, which is where it matters (A-R5-3).
        Box::new(setup_cache_dir::SetupCacheDir::with_ops(make_ops())),
        Box::new(apt_packages::AptPackagesStep::bootstrap_with_ops(make_ops())),
        Box::new(apt_packages::AptPackagesStep::odoo_dependencies_with_ops(
            make_ops(),
        )),
        Box::new(install_wkhtmltopdf::InstallWkhtmltopdf::with_parts(
            make_ops(),
            Box::new(RealDownloader::new()) as Box<dyn Downloader>,
            install_wkhtmltopdf::default_checksums(),
            std::env::temp_dir(),
        )),
        // undone in reverse: database, then role, then the service.
        Box::new(setup_postgres::SetupPostgres::with_ops(make_ops())),
        Box::new(create_db_role::CreateDbRole::with_ops(make_ops())),
        Box::new(create_database::CreateDatabase::with_ops(make_ops())),
        // sources: clone → venv → pip. pip's undo is a no-op, absorbed by the
        // venv's. `for_run` because the production clone retries with backoff.
        Box::new(clone_odoo_repo::CloneOdooRepo::for_run(make_ops())),
        Box::new(create_virtualenv::CreateVirtualenv::with_ops(make_ops())),
        Box::new(install_python_requirements::InstallPythonRequirements::with_ops(make_ops())),
        // the init's undo is a no-op: the dropdb cleans the schema up.
        Box::new(generate_config::GenerateConfig::with_ops(make_ops())),
        // the filestore must be created before Odoo creates it itself, or it is
        // unrecorded and un-undoable (A-R5-3). after `create-database`, whose
        // verdict it reads.
        Box::new(setup_data_dir::SetupDataDir::with_ops(make_ops())),
        Box::new(initialize_odoo_database::InitializeOdooDatabase::with_ops(
            make_ops(),
        )),
        // undone as stop → disable → rm → daemon-reload. `for_run` waits for
        // the service to settle in production.
        Box::new(setup_systemd::SetupSystemd::for_run(make_ops())),
        // nginx, gated: install → vhost → enable → selinux → firewall → reload.
        Box::new(nginx_install::NginxInstall::with_ops(make_ops())),
        Box::new(nginx_write_config::NginxWriteConfig::with_ops(make_ops())),
        Box::new(nginx_enable_site::NginxEnableSite::with_ops(make_ops())),
        // SELinux before the reload: without the boolean nginx reloads fine and
        // then **answers 502**, because the policy denies it the connection. a
        // no-op where SELinux is not in use.
        Box::new(nginx_selinux::NginxSelinux::with_ops(make_ops())),
        Box::new(nginx_firewall::NginxFirewall::with_ops(make_ops())),
        Box::new(nginx_reload::NginxReload::with_ops(make_ops())),
        // the `odoo` helper command, plus the PATH line in the user's
        // `.bashrc`.
        Box::new(write_control_script::WriteControlScript::with_ops(
            make_ops(),
        )),
        Box::new(patch_bashrc::PatchBashrc::with_ops(make_ops())),
    ]
}

/// rebuilds a step from its **persisted name**, with injectable `SystemOps`.
///
/// the "identity" half of rehydration: this produces the object and
/// [`crate::step::Step::rehydrate`] puts its state back. together they make
/// executable the undo of an installation this process never ran.
///
/// `None` for an unknown name — a state written by a version with steps that no
/// longer exist here. the caller reports it as a leftover rather than failing:
/// the other steps still need undoing.
///
/// the wkhtmltopdf downloader and checksum table are always the production
/// ones: the undo purges a package and downloads nothing. for the same reason
/// the clone and systemd steps are built without their run-time parameters —
/// removing a directory and stopping a service need no waiting.
pub fn step_by_name(name: &str, make_ops: OpsFactory<'_>) -> Option<Box<dyn Step>> {
    let step: Box<dyn Step> = match name {
        "prepare-opt-root" => Box::new(prepare_opt_root::PrepareOptRoot::with_ops(make_ops())),
        "create-odoo-user" => Box::new(create_odoo_user::CreateOdooUser::with_ops(make_ops())),
        "setup-log-dir" => Box::new(setup_log_dir::SetupLogDir::with_ops(make_ops())),
        "setup-cache-dir" => Box::new(setup_cache_dir::SetupCacheDir::with_ops(make_ops())),
        "bootstrap-prerequisites" => {
            Box::new(apt_packages::AptPackagesStep::bootstrap_with_ops(make_ops()))
        }
        "install-system-dependencies" => Box::new(
            apt_packages::AptPackagesStep::odoo_dependencies_with_ops(make_ops()),
        ),
        "install-wkhtmltopdf" => Box::new(install_wkhtmltopdf::InstallWkhtmltopdf::with_parts(
            make_ops(),
            Box::new(RealDownloader::new()) as Box<dyn Downloader>,
            install_wkhtmltopdf::default_checksums(),
            std::env::temp_dir(),
        )),
        "setup-postgres" => Box::new(setup_postgres::SetupPostgres::with_ops(make_ops())),
        "create-db-role" => Box::new(create_db_role::CreateDbRole::with_ops(make_ops())),
        "create-database" => Box::new(create_database::CreateDatabase::with_ops(make_ops())),
        "clone-odoo-repo" => Box::new(clone_odoo_repo::CloneOdooRepo::with_ops(make_ops())),
        "create-virtualenv" => Box::new(create_virtualenv::CreateVirtualenv::with_ops(make_ops())),
        "install-python-requirements" => {
            Box::new(install_python_requirements::InstallPythonRequirements::with_ops(make_ops()))
        }
        "generate-config" => Box::new(generate_config::GenerateConfig::with_ops(make_ops())),
        "setup-data-dir" => Box::new(setup_data_dir::SetupDataDir::with_ops(make_ops())),
        "initialize-odoo-database" => Box::new(
            initialize_odoo_database::InitializeOdooDatabase::with_ops(make_ops()),
        ),
        "setup-systemd" => Box::new(setup_systemd::SetupSystemd::with_ops(make_ops())),
        "nginx-install" => Box::new(nginx_install::NginxInstall::with_ops(make_ops())),
        "nginx-write-config" => {
            Box::new(nginx_write_config::NginxWriteConfig::with_ops(make_ops()))
        }
        "nginx-enable-site" => Box::new(nginx_enable_site::NginxEnableSite::with_ops(make_ops())),
        "nginx-selinux" => Box::new(nginx_selinux::NginxSelinux::with_ops(make_ops())),
        "nginx-firewall" => Box::new(nginx_firewall::NginxFirewall::with_ops(make_ops())),
        "nginx-reload" => Box::new(nginx_reload::NginxReload::with_ops(make_ops())),
        "write-control-script" => Box::new(write_control_script::WriteControlScript::with_ops(
            make_ops(),
        )),
        "patch-bashrc" => Box::new(patch_bashrc::PatchBashrc::with_ops(make_ops())),
        _ => return None,
    };
    Some(step)
}

/// the canonical sequence's step names, in execution order.
///
/// derived from [`build_steps`] rather than written by hand: a parallel list
/// would go stale with nothing noticing, and the cost here is one pass of
/// side-effect-free constructors.
pub fn canonical_step_names(make_ops: OpsFactory<'_>) -> Vec<String> {
    build_steps(make_ops)
        .iter()
        .map(|s| s.name().to_string())
        .collect()
}

pub mod apt_packages;
pub mod clone_odoo_repo;
pub mod create_database;
pub mod create_db_role;
pub mod create_odoo_user;
pub mod create_virtualenv;
pub mod generate_config;
pub mod initialize_odoo_database;
pub mod install_python_requirements;
pub mod install_wkhtmltopdf;
pub mod nginx_enable_site;
pub mod nginx_firewall;
pub mod nginx_install;
pub mod nginx_reload;
pub mod nginx_selinux;
pub mod nginx_write_config;
pub mod noop;
pub mod patch_bashrc;
pub mod prepare_opt_root;
pub mod setup_cache_dir;
pub mod setup_data_dir;
pub mod setup_log_dir;
pub mod setup_postgres;
pub mod setup_systemd;
pub mod write_control_script;
