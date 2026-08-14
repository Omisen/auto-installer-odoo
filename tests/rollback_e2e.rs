//! **end-to-end** rollback tests (G7): the surgical promise proven at system
//! level.
//!
//! start from an initial [`SystemModel`] state, run the real step sequence,
//! inject a failure, and check the state afterwards is **identical** — and that
//! the customer's pre-existing resources survive intact.

mod common;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use common::model::{ModelState, SystemModel};
use common::MockDownloader;
use invok::checks::OsInfo;
use invok::context::Context;
use invok::engine::Installer;
use invok::error::StepError;
use invok::secret::Secret;
use invok::step::Step;
use invok::steps::apt_packages::AptPackagesStep;
use invok::steps::clone_odoo_repo::CloneOdooRepo;
use invok::steps::create_database::CreateDatabase;
use invok::steps::create_db_role::CreateDbRole;
use invok::steps::create_odoo_user::CreateOdooUser;
use invok::steps::create_virtualenv::CreateVirtualenv;
use invok::steps::generate_config::GenerateConfig;
use invok::steps::initialize_odoo_database::InitializeOdooDatabase;
use invok::steps::install_wkhtmltopdf::InstallWkhtmltopdf;
use invok::steps::nginx_enable_site::NginxEnableSite;
use invok::steps::nginx_firewall::NginxFirewall;
use invok::steps::nginx_install::NginxInstall;
use invok::steps::nginx_reload::NginxReload;
use invok::steps::nginx_write_config::NginxWriteConfig;
use invok::steps::noop::NoopStep;
use invok::steps::patch_bashrc::PatchBashrc;
use invok::steps::setup_systemd::SetupSystemd;
use invok::steps::write_control_script::WriteControlScript;

const HOME: &str = "/opt/odoo";
const INSTALL: &str = "/opt/odoo/odoo18";
const SUDO_HOME: &str = "/home/alice";
const BASHRC: &str = "/home/alice/.bashrc";
const BASHRC_ORIG: &str = "alias ll='ls -la'\nexport EDITOR=vim\n";

fn set(items: &[&str]) -> HashSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}
fn paths(items: &[&str]) -> HashSet<PathBuf> {
    items.iter().map(PathBuf::from).collect()
}

/// a "fresh machine" initial state: the home directories exist, the user's
/// `.bashrc` has its own contents, and nothing of ours.
fn fresh_state() -> ModelState {
    let mut contents = HashMap::new();
    contents.insert(PathBuf::from(BASHRC), BASHRC_ORIG.to_string());
    ModelState {
        paths: paths(&[HOME, SUDO_HOME, BASHRC]),
        file_contents: contents,
        packages: set(&["coreutils"]),
        sudo_home: Some(SUDO_HOME.to_string()),
        ..Default::default()
    }
}

fn ctx(state_path: PathBuf, aggressive: bool) -> Context {
    Context {
        odoo_user: "odoo".to_string(),
        db_user: "odoo".to_string(),
        db_name: "odoo".to_string(),
        db_password: Secret::default(),
        admin_passwd: Secret::new("s3cret"),
        odoo_version: "18.0".to_string(),
        odoo_version_short: "18".to_string(),
        odoo_home: PathBuf::from(HOME),
        install_dir: PathBuf::from(INSTALL),
        port: 8069,
        with_nginx: false,
        dry_run: false,
        aggressive_rollback: aggressive,
        sudo_user: Some("alice".to_string()),
        state_path,
        ..Default::default()
    }
}

/// the real step sequence, sharing one model.
///
/// the first step is excluded — it uses the filesystem directly and has its own
/// tests — so the home is part of the initial state here.
fn chain(model: &SystemModel) -> Vec<Box<dyn Step>> {
    vec![
        Box::new(CreateOdooUser::with_ops(model.boxed())),
        Box::new(AptPackagesStep::odoo_dependencies_with_ops(model.boxed())),
        Box::new(SetupPostgres::with_ops(model.boxed())),
        Box::new(CreateDbRole::with_ops(model.boxed())),
        Box::new(CreateDatabase::with_ops(model.boxed())),
        Box::new(CloneOdooRepo::with_ops(model.boxed())),
        Box::new(CreateVirtualenv::with_ops(model.boxed())),
        Box::new(GenerateConfig::with_ops(model.boxed())),
        Box::new(InitializeOdooDatabase::with_ops(model.boxed())),
        Box::new(SetupSystemd::with_ops(model.boxed())),
        Box::new(WriteControlScript::with_ops(model.boxed())),
        Box::new(PatchBashrc::with_ops(model.boxed())),
    ]
}

use invok::steps::setup_postgres::SetupPostgres;

#[test]
fn full_chain_failure_returns_to_virgin_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let model = SystemModel::new(fresh_state());
    let initial = model.snapshot();

    // the whole sequence plus a failing step at the end: everything rolls back.
    let mut steps = chain(&model);
    steps.push(Box::new(NoopStep::new("boom").fail_on_run()));

    let ctx = ctx(dir.path().join("state.json"), /* aggressive */ true);
    let mut installer = Installer::new();
    assert!(
        installer.execute(&mut steps, &ctx).is_err(),
        "the failure triggers the rollback"
    );

    assert_eq!(
        model.snapshot(),
        initial,
        "after the rollback the system is virgin again"
    );
    // the user's `.bashrc` is byte for byte as before.
    assert_eq!(
        model
            .snapshot()
            .file_contents
            .get(&PathBuf::from(BASHRC))
            .map(String::as_str),
        Some(BASHRC_ORIG)
    );
}

#[test]
fn mid_chain_failure_returns_to_virgin_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let model = SystemModel::new(fresh_state());
    let initial = model.snapshot();

    // a failure partway: the first five steps complete, then the failure is
    // injected.
    let mut steps: Vec<Box<dyn Step>> = chain(&model).into_iter().take(5).collect();
    steps.push(Box::new(NoopStep::new("boom").fail_on_run()));

    let ctx = ctx(dir.path().join("state.json"), true);
    let mut installer = Installer::new();
    assert!(installer.execute(&mut steps, &ctx).is_err());

    assert_eq!(
        model.snapshot(),
        initial,
        "the user, deps, postgres, role and DB we created must all go"
    );
}

#[test]
fn preexisting_resources_survive_rollback() {
    // an initial state with THE CUSTOMER'S RESOURCES: PostgreSQL installed and
    // running, and a database of that name already there. the init hard-stops,
    // the chain fails, and the rollback touches none of it.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut init = fresh_state();
    init.packages.insert("postgresql".to_string());
    init.packages.insert("postgresql-contrib".to_string());
    init.svc_enabled.insert("postgresql".to_string());
    init.svc_active.insert("postgresql".to_string());
    init.pg_dbs.insert("odoo".to_string()); // the customer's pre-existing database

    let model = SystemModel::new(init);
    let initial = model.snapshot();

    let mut steps = chain(&model);
    let ctx = ctx(dir.path().join("state.json"), false);
    let mut installer = Installer::new();
    let result = installer.execute(&mut steps, &ctx);
    assert!(
        result.is_err(),
        "the init hard stop must halt the chain on a pre-existing DB"
    );

    let final_state = model.snapshot();
    // everything is back as it was.
    assert_eq!(final_state, initial, "pre-existing resources stay, ours go");
    // explicit checks of the critical protections, at chain level:
    assert!(
        final_state.pg_dbs.contains("odoo"),
        "the customer's DB must NOT be dropped"
    );
    assert!(
        final_state.packages.contains("postgresql"),
        "PostgreSQL preinstallato resta"
    );
    assert!(
        final_state.svc_active.contains("postgresql"),
        "a service already active stays active (D4)"
    );
    assert!(
        final_state.paths.contains(&PathBuf::from(HOME)),
        "/opt/odoo preesistente resta"
    );
}

// --- nginx in the full chain (A4.2) -----------------------------------------
//
// the nginx steps had isolated unit tests only. here they run in the real
// chain, in their real position, and their rollback interleaves with the
// others'. this is where the mutations on **third-party** configuration live:
// the customer's default site and firewall rules.

const VHOST: &str = "/etc/nginx/sites-available/odoo18";
const SITE_LINK: &str = "/etc/nginx/sites-enabled/odoo18";
const DEFAULT_SITE: &str = "/etc/nginx/sites-enabled/default";

/// a step that **photographs** the model mid-chain and then fails, triggering
/// the rollback.
///
/// this keeps the assertions honest: checking only the *final* state cannot
/// tell "mutated then restored" from "never touched". the intermediate photo
/// proves the first half, the final state proves the second.
struct SpyThenFail {
    model: SystemModel,
    seen: std::sync::Arc<std::sync::Mutex<Option<ModelState>>>,
}

impl SpyThenFail {
    #[allow(clippy::type_complexity)]
    fn new(model: &SystemModel) -> (Self, std::sync::Arc<std::sync::Mutex<Option<ModelState>>>) {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        (
            SpyThenFail {
                model: model.handle(),
                seen: std::sync::Arc::clone(&seen),
            },
            seen,
        )
    }
}

impl Step for SpyThenFail {
    fn name(&self) -> &str {
        "spy-then-fail"
    }
    fn snapshot(&mut self, _ctx: &Context) -> Result<(), StepError> {
        Ok(())
    }
    fn run(&mut self, _ctx: &Context) -> Result<(), StepError> {
        if let Ok(mut slot) = self.seen.lock() {
            *slot = Some(self.model.snapshot());
        }
        Err(StepError::Precondition(
            "fallimento iniettato a valle della fase Nginx".to_string(),
        ))
    }
    fn undo(&self, _ctx: &Context) -> Result<(), StepError> {
        Ok(())
    }
    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::Value::Null
    }
}

/// reads the photograph taken by [`SpyThenFail`].
fn photo(slot: &std::sync::Arc<std::sync::Mutex<Option<ModelState>>>) -> ModelState {
    slot.lock()
        .ok()
        .and_then(|s| s.clone())
        .expect("the spy step must have run")
}

/// as [`ctx`], with the nginx phase enabled.
fn ctx_nginx(state_path: PathBuf, aggressive: bool, ssl: bool) -> Context {
    Context {
        with_nginx: true,
        nginx_server_name: "_".to_string(),
        nginx_open_https_port: ssl,
        ..ctx(state_path, aggressive)
    }
}

/// an initial state with the firewall installed **and active**: without it the
/// step is a declared no-op and the delta would never be exercised.
fn fresh_state_with_ufw() -> ModelState {
    ModelState {
        ufw_available: true,
        ufw_active: true,
        ..fresh_state()
    }
}

/// the real chain **including** the nginx steps, in their production position:
/// after the service, before the control script.
fn chain_with_nginx(model: &SystemModel) -> Vec<Box<dyn Step>> {
    let mut steps = chain(model);
    let at = steps
        .iter()
        .position(|s| s.name() == "setup-systemd")
        .map(|i| i + 1)
        .unwrap_or(steps.len());
    let nginx: Vec<Box<dyn Step>> = vec![
        Box::new(NginxInstall::with_ops(model.boxed())),
        Box::new(NginxWriteConfig::with_ops(model.boxed())),
        Box::new(NginxEnableSite::with_ops(model.boxed())),
        Box::new(NginxFirewall::with_ops(model.boxed())),
        Box::new(NginxReload::with_ops(model.boxed())),
    ];
    steps.splice(at..at, nginx);
    steps
}

#[test]
fn full_chain_with_nginx_failure_returns_to_virgin_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let model = SystemModel::new(fresh_state_with_ufw());
    let initial = model.snapshot();

    let mut steps = chain_with_nginx(&model);
    let (spy, seen) = SpyThenFail::new(&model);
    steps.push(Box::new(spy));

    let ctx = ctx_nginx(dir.path().join("state.json"), true, /* ssl */ false);
    let mut installer = Installer::new();
    assert!(installer.execute(&mut steps, &ctx).is_err());

    // first half: the nginx phase really did mutate the system.
    let mid = photo(&seen);
    assert!(mid.packages.contains("nginx"), "nginx installato");
    assert!(mid.paths.contains(&PathBuf::from(VHOST)), "vhost scritto");
    assert!(
        mid.symlinks.contains(&PathBuf::from(SITE_LINK)),
        "sito abilitato"
    );
    assert!(mid.ufw_rules.contains("80/tcp"), "porta 80 aperta");
    assert!(mid.svc_active.contains("nginx"), "nginx avviato");

    // second half: the rollback undid it all.
    let final_state = model.snapshot();
    assert_eq!(
        final_state, initial,
        "the rollback brings the nginx phase back to virgin too"
    );
    // explicit checks on each nginx artifact: a failing equality would not say
    // *which* one remained.
    assert!(
        !final_state.packages.contains("nginx"),
        "an nginx we installed is purged with --aggressive-rollback"
    );
    assert!(!final_state.paths.contains(&PathBuf::from(VHOST)), "vhost");
    assert!(
        !final_state.symlinks.contains(&PathBuf::from(SITE_LINK)),
        "symlink sites-enabled"
    );
    assert!(final_state.ufw_rules.is_empty(), "regole ufw del delta");
    assert!(
        !final_state.svc_active.contains("nginx") && !final_state.svc_enabled.contains("nginx"),
        "an nginx service we started and enabled is stopped and disabled"
    );
}

#[test]
fn preexisting_default_site_is_restored_by_the_full_chain_rollback() {
    // the phase's most treacherous mutation: freeing port 80 means moving the
    // customer's default site aside. tested in isolation until now; here the
    // restoration happens at the end of a complete rollback.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut init = fresh_state_with_ufw();
    // a customer with nginx installed, running, and their default site.
    init.packages.insert("nginx".to_string());
    init.svc_enabled.insert("nginx".to_string());
    init.svc_active.insert("nginx".to_string());
    init.symlinks.insert(PathBuf::from(DEFAULT_SITE));
    // nginx is running and serving that default site: the **loaded** config
    // matches the one on disk.
    init.nginx_loaded_sites = Some([PathBuf::from(DEFAULT_SITE)].into_iter().collect());

    let model = SystemModel::new(init);
    let initial = model.snapshot();

    let mut steps = chain_with_nginx(&model);
    let (spy, seen) = SpyThenFail::new(&model);
    steps.push(Box::new(spy));

    // aggressive on purpose: not even the most aggressive rollback may touch an
    // nginx that was already the customer's.
    let ctx = ctx_nginx(dir.path().join("state.json"), true, false);
    let mut installer = Installer::new();
    assert!(installer.execute(&mut steps, &ctx).is_err());

    // first half: the default site really was removed to free the port, and
    // ours enabled in its place.
    let mid = photo(&seen);
    assert!(
        !mid.symlinks.contains(&PathBuf::from(DEFAULT_SITE)),
        "during the installation the default site is removed (port 80 freed)"
    );
    assert!(
        mid.symlinks.contains(&PathBuf::from(SITE_LINK)),
        "our site is enabled"
    );

    // second half: the rollback stitched the customer's config back.
    let final_state = model.snapshot();
    assert!(
        final_state.symlinks.contains(&PathBuf::from(DEFAULT_SITE)),
        "the customer's default site must be restored by the rollback"
    );
    assert!(
        final_state.packages.contains("nginx"),
        "a pre-existing nginx is NOT purged, not even with --aggressive-rollback"
    );
    assert!(
        final_state.svc_active.contains("nginx") && final_state.svc_enabled.contains("nginx"),
        "an nginx already active and enabled stays that way (D4)"
    );
    // putting the **files** back is not enough: nginx keeps serving what it has
    // in memory. left with our config loaded, the customer's site stays down
    // until someone reloads by hand — a rollback complete on paper and broken
    // in production.
    assert_eq!(
        final_state.nginx_loaded_sites,
        Some([PathBuf::from(DEFAULT_SITE)].into_iter().collect()),
        "after the rollback nginx must **serve** the restored config, not ours"
    );
    assert_eq!(final_state, initial, "final state equals initial state");
}

#[test]
fn only_our_ufw_delta_is_removed_by_the_full_chain_rollback() {
    // the customer had already opened one port; we open another. the rollback
    // must remove **only** ours.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut init = fresh_state_with_ufw();
    init.ufw_rules.insert("80/tcp".to_string());

    let model = SystemModel::new(init);
    let initial = model.snapshot();

    let mut steps = chain_with_nginx(&model);
    let (spy, seen) = SpyThenFail::new(&model);
    steps.push(Box::new(spy));

    let ctx = ctx_nginx(dir.path().join("state.json"), true, /* ssl */ true);
    let mut installer = Installer::new();
    assert!(installer.execute(&mut steps, &ctx).is_err());

    // first half: the rule really was opened by us.
    let mid = photo(&seen);
    assert!(
        mid.ufw_rules.contains("443/tcp"),
        "the delta's rule is opened during the installation"
    );

    // second half: the rollback removes only what it opened.
    let final_state = model.snapshot();
    assert!(
        final_state.ufw_rules.contains("80/tcp"),
        "the customer's pre-existing rule stays open"
    );
    assert!(
        !final_state.ufw_rules.contains("443/tcp"),
        "the rule we added (the delta) is removed"
    );
    assert_eq!(final_state, initial, "final state equals initial state");
}

#[test]
fn full_chain_with_nginx_succeeds_and_leaves_the_expected_artifacts() {
    // the **run** side of the nginx phase in the real chain: until now no
    // end-to-end test reached the end without an injected failure.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut init = fresh_state_with_ufw();
    init.symlinks.insert(PathBuf::from(DEFAULT_SITE));
    let model = SystemModel::new(init);

    let mut steps = chain_with_nginx(&model);
    let ctx = ctx_nginx(dir.path().join("state.json"), false, /* ssl */ true);
    let mut installer = Installer::new();
    installer
        .execute(&mut steps, &ctx)
        .expect("the full chain with nginx must reach the end");

    let s = model.snapshot();
    assert!(s.packages.contains("nginx"));
    assert!(s.paths.contains(&PathBuf::from(VHOST)), "vhost scritto");
    assert!(
        s.symlinks.contains(&PathBuf::from(SITE_LINK)),
        "sito abilitato"
    );
    assert!(s.svc_active.contains("nginx"), "nginx avviato");
    assert!(s.ufw_rules.contains("80/tcp") && s.ufw_rules.contains("443/tcp"));
    assert!(
        !s.symlinks.contains(&PathBuf::from(DEFAULT_SITE)),
        "on a successful installation the default site stays removed: our vhost needs \
         port 80"
    );
}

#[test]
fn nginx_steps_are_inert_in_the_full_chain_without_with_nginx() {
    // gating in the full chain: the same steps, disabled, must leave **no**
    // trace after a successful installation.
    let dir = tempfile::tempdir().expect("tempdir");
    let model = SystemModel::new(fresh_state_with_ufw());

    let mut steps = chain_with_nginx(&model);
    let ctx = ctx(dir.path().join("state.json"), false); // with_nginx = false
    let mut installer = Installer::new();
    installer
        .execute(&mut steps, &ctx)
        .expect("a chain without nginx");

    let s = model.snapshot();
    assert!(!s.packages.contains("nginx"), "no nginx package");
    assert!(!s.paths.contains(&PathBuf::from(VHOST)), "no vhost");
    assert!(
        !s.symlinks.contains(&PathBuf::from(SITE_LINK)),
        "no site enabled"
    );
    assert!(s.ufw_rules.is_empty(), "nessuna regola firewall");
    assert!(!s.svc_enabled.contains("nginx") && !s.svc_active.contains("nginx"));
}

// --- a field regression: a broken package database (A-RT-1, A-RT-2) ---------
//
// observed on a clean VM: the wkhtmltopdf package has system dependencies the
// machine lacks, the direct install fails leaving the database inconsistent,
// the chain stops — and the rollback cannot purge the delta because the manager
// refuses to operate. the system stays dirty: the surgical promise broken in
// the very scenario it matters most.

/// the wkhtmltopdf package's system dependencies on a minimal VM.
const WK_SYSTEM_DEPS: &[&str] = &["fontconfig", "libxrender1", "xfonts-75dpi", "xfonts-base"];

/// a test step that breaks the package database and then fails: models "a
/// downstream step left the system inconsistent before the rollback ran".
struct BreakDpkgThenFail {
    model: SystemModel,
}

impl Step for BreakDpkgThenFail {
    fn name(&self) -> &str {
        "break-dpkg"
    }
    fn snapshot(&mut self, _ctx: &Context) -> Result<(), StepError> {
        Ok(())
    }
    fn run(&mut self, _ctx: &Context) -> Result<(), StepError> {
        self.model.mutate(|s| s.dpkg_broken = true);
        Err(StepError::Precondition(
            "a step failed leaving dpkg inconsistent".to_string(),
        ))
    }
    fn undo(&self, _ctx: &Context) -> Result<(), StepError> {
        Ok(())
    }
    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::Value::Null
    }
}

#[test]
fn rollback_cleans_the_apt_delta_even_with_a_broken_dpkg() {
    let dir = tempfile::tempdir().expect("tempdir");
    let model = SystemModel::new(fresh_state());
    let initial = model.snapshot();

    // the real chain: the system packages are installed, then a downstream step
    // dies leaving the database broken.
    let mut steps = chain(&model);
    steps.push(Box::new(BreakDpkgThenFail {
        model: model.handle(),
    }));

    let ctx = ctx(dir.path().join("state.json"), true);
    let mut installer = Installer::new();
    assert!(installer.execute(&mut steps, &ctx).is_err());

    let final_state = model.snapshot();
    // the heart of the regression: before the fix these packages stayed
    // installed, because the purge failed on a broken database.
    for pkg in ["build-essential", "python3-dev", "libpq-dev"] {
        assert!(
            !final_state.packages.contains(pkg),
            "'{pkg}' from the delta should have been purged: the rollback must recover dpkg \
             before giving up"
        );
    }
    assert!(
        !final_state.dpkg_broken,
        "the rollback must leave dpkg consistent"
    );
    assert_eq!(
        final_state, initial,
        "even starting from a broken dpkg the system comes back virgin"
    );
}

// --- wkhtmltopdf in the full chain (A4.2) -----------------------------------

#[test]
fn wkhtmltopdf_is_installed_in_the_chain_and_purged_by_the_rollback() {
    // the step tests cover download → verify → install but **not** the undo:
    // here the happy path runs in the chain and the rollback must return the
    // system to "not installed".
    let dir = tempfile::tempdir().expect("tempdir");
    // a minimal VM like the test one: the package's system dependencies are
    // missing and must be resolved by the installation (A-RT-1).
    let mut init = fresh_state();
    init.pending_deps = WK_SYSTEM_DEPS.iter().map(|s| s.to_string()).collect();
    let model = SystemModel::new(init);
    let initial = model.snapshot();
    assert!(initial.wk_version.is_none(), "we start without wkhtmltopdf");

    // a fake package whose hash is, by construction, the expected one.
    let bytes = b"contenuto .deb di prova".to_vec();
    let probe = dir.path().join("probe.bin");
    std::fs::write(&probe, &bytes).expect("probe");
    let sha = invok::system_ops::sha256_hex(&probe).expect("hash");
    let checksums = [("jammy".to_string(), sha)].into_iter().collect();

    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let wk = InstallWkhtmltopdf::with_parts(
        model.boxed(),
        Box::new(MockDownloader::new(bytes, log)),
        checksums,
        dir.path().to_path_buf(),
    );

    // the real position: right after the system dependencies.
    let mut steps = chain(&model);
    let at = steps
        .iter()
        .position(|s| s.name() == "apt-packages")
        .map(|i| i + 1)
        .unwrap_or(0);
    steps.insert(at, Box::new(wk));
    let (spy, seen) = SpyThenFail::new(&model);
    steps.push(Box::new(spy));

    let mut c = ctx(dir.path().join("state.json"), true);
    c.os_info = Some(OsInfo {
        id: "ubuntu".to_string(),
        version: "22.04".to_string(),
        codename: Some("jammy".to_string()),
        family: invok::distro::OsFamily::Debian,
    });

    let mut installer = Installer::new();
    assert!(installer.execute(&mut steps, &c).is_err());

    // first half: the happy path reached the install. a checksum mismatch would
    // have failed the step earlier.
    let mid = photo(&seen);
    assert!(
        mid.packages.contains("wkhtmltox"),
        "download → checksum verificato → installato"
    );
    assert_eq!(mid.wk_version.as_deref(), Some("0.12.6.1"));
    // A-RT-1: the package's dependencies were resolved, not ignored. the direct
    // install would have failed here.
    for dep in WK_SYSTEM_DEPS {
        assert!(
            mid.packages.contains(*dep),
            "'{dep}' should have been installed as a dependency of the .deb"
        );
    }

    // second half: the rollback purges the package but **not** its system
    // dependencies, which stay like the bootstrap utilities (D3).
    let final_state = model.snapshot();
    assert!(
        !final_state.packages.contains("wkhtmltox"),
        "a wkhtmltopdf we installed is purged by the rollback"
    );
    assert!(final_state.wk_version.is_none());
    for dep in WK_SYSTEM_DEPS {
        assert!(
            final_state.packages.contains(*dep),
            "'{dep}' is a system library: the rollback leaves it (D3)"
        );
    }
}
