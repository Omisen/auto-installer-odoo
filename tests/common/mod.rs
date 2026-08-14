//! the [`SystemOps`] mock shared by the privileged steps' tests.
//!
//! it executes nothing: it records the requested operations in a shared log and
//! answers queries from a static configuration. that lets the tests check the
//! decision logic — which command, with which arguments, in which `PreState`
//! branch — without root and without mutating anything.

#![allow(dead_code)] // non tutti i test usano tutte le utility

pub mod model;

use std::cell::Cell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use invok::distro::{Distro, Firewall, OsFamily};
use invok::error::StepError;
use invok::packaging::{Availability, PackageCatalog, PackageManager};
use invok::progress::ProgressReporter;
use invok::system_ops::{Downloader, OdooSourceState, OwnerId, PathKind, SystemOps, UserSpec};

/// a mutating operation recorded by the mock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    CreateUser(UserSpec),
    DeleteUser(String),
    DeleteGroup(String),
    ChownNamed {
        path: PathBuf,
        owner: String,
        group: String,
    },
    ChownNumeric {
        path: PathBuf,
        id: OwnerId,
    },
    Chmod {
        path: PathBuf,
        mode: u32,
    },
    Mkdir(PathBuf),
    Rmdir(PathBuf),
    PkgRefreshIndex,
    PkgInstall(Vec<String>),
    PkgRemove(Vec<String>),
    PkgRemoveOrphans,
    PkgRepair,
    PkgDeepRepair,
    PkgInstallLocalFile(PathBuf),
    Download {
        url: String,
        dest: PathBuf,
    },
    ServiceEnable(String),
    ServiceDisable(String),
    ServiceStart(String),
    ServiceStop(String),
    ServiceRestart(String),
    ServiceReload(String),
    DaemonReload,
    CreateSymlink {
        src: PathBuf,
        link: PathBuf,
    },
    RemoveSymlink(PathBuf),
    UfwAllow(String),
    InitPostgresCluster,
    SetSelinuxBoolean {
        boolean: String,
        value: bool,
    },
    UfwDelete(String),
    ChownToUser {
        path: PathBuf,
        user: String,
    },
    AppendLine(PathBuf),
    PgCreateRole {
        role: String,
        // only whether a password is present — NEVER the value.
        has_password: bool,
    },
    PgDropRole(String),
    CreateDb {
        owner: String,
        db: String,
    },
    DropDb(String),
    RunAsUser {
        user: String,
        program: String,
        args: Vec<String>,
    },
    MkdirAsUser {
        user: String,
        path: PathBuf,
    },
    RemoveDirAll(PathBuf),
    GitClone {
        target: PathBuf,
        branch: String,
        depth: u32,
    },
    TarballInstall {
        target: PathBuf,
    },
    CreateVenv {
        /// the interpreter the venv was created on (M11): without recording
        /// it, no test could notice the wrong one being used.
        python: String,
        venv: PathBuf,
    },
    /// "which version is this interpreter?", carrying the name asked about —
    /// the part that matters, since A-MD-7's diagnosis must speak of the venv's
    /// Python, not the system's.
    PythonVersion(String),
    WritePrivateFile(PathBuf),
    CreatePrivateFile(PathBuf),
    MoveFile {
        src: PathBuf,
        dst: PathBuf,
    },
    CopyFile {
        src: PathBuf,
        dst: PathBuf,
    },
    RemoveFile(PathBuf),
    OdooInitBase {
        conf: PathBuf,
        db: String,
    },
}

/// the mock's static answers to state queries.
#[derive(Debug, Clone)]
pub struct MockConfig {
    pub user_exists: bool,
    pub path_exists: bool,
    pub owner: OwnerId,
    pub dir_empty: bool,
    /// packages considered already installed.
    pub installed_packages: HashSet<String>,
    /// packages with NO installable candidate: models a name that does not
    /// exist on this release (A5.1). empty means everything is installable.
    pub packages_without_candidate: HashSet<String>,
    /// **virtual** packages: no real candidate, but the manager resolves them
    /// through a provider (A5.1-bis).
    pub virtual_packages: HashSet<String>,
    /// is the index populated? `false` models a machine never refreshed, where
    /// **no** name has a candidate until it is — the state that produced the
    /// A5.1-bis false positive.
    pub apt_index_populated: bool,
    /// the reported wkhtmltopdf version; `None` means not installed.
    pub wk_version: Option<String>,
    /// the service's initial state: enabled and active.
    pub service_enabled: bool,
    pub service_active: bool,
    /// when set, starting does NOT make the service active.
    pub service_start_fails: bool,
    /// whether the PostgreSQL role and database already exist.
    pub role_exists: bool,
    pub db_exists: bool,
    /// the non-template databases listed, for the cluster caution.
    pub pg_databases_list: Vec<String>,
    /// the detected state of the Odoo sources.
    pub source_state: OdooSourceState,
    /// how many clone attempts fail before one succeeds.
    pub git_clone_fail_times: u32,
    /// when set, the tarball fallback fails.
    pub tarball_fails: bool,
    /// when set, simulated network failures are **timeouts** rather than
    /// non-zero exits, to check a timeout is treated as retryable like any
    /// other failure.
    pub network_failures_are_timeouts: bool,
    /// does the venv's interpreter already exist?
    pub venv_exists: bool,
    /// is virtualenv creation available?
    pub venv_available: bool,
    /// the PGDATA the service unit declares (A-MD-6). `None` means "unknown",
    /// the normal case in tests.
    pub pg_declared_data_dir: Option<PathBuf>,
    /// fails the user-run whose arguments contain this fragment.
    pub run_as_user_fails_on: Option<String>,
    /// the interpreter's version, or `None` for "unknown" (A-MD-7).
    ///
    /// the default is a Python **covered** by Odoo's pins, so the diagnosis
    /// never appears in tests that are not about it.
    pub python_version: Option<(u32, u32)>,
    /// the requirements file's contents; `None` makes reading it fail.
    pub requirements_content: Option<String>,
    /// is the Odoo schema already in the database?
    pub db_initialized: bool,
    /// when set, file operations touch the real filesystem — tempdir paths
    /// only. the chown stays simulated.
    pub real_fs: bool,
    /// does the nginx default site exist?
    pub default_site_exists: bool,
    /// **what** is at the default site (A-V3-5).
    ///
    /// `None` stays consistent with the boolean above. set explicitly to model
    /// the cases that boolean could not tell apart: a regular file, or a
    /// symlink towards a non-standard target.
    pub default_site_kind: Option<PathKind>,
    /// does our own enabling symlink already exist?
    pub our_link_exists: bool,
    /// the firewall: installed, active, and its existing rules.
    pub ufw_available: bool,
    pub ufw_active: bool,
    pub existing_ufw_rules: HashSet<String>,
    /// does the config validate?
    pub nginx_test_ok: bool,
    /// the home returned for the user; `None` means not found.
    pub sudo_home: Option<String>,
    /// the package database starts inconsistent, so the manager refuses to
    /// operate until a repair fixes it — the state observed on the test VM.
    pub dpkg_broken: bool,
    /// the first repair cannot fix it.
    pub fix_broken_fails: bool,
    /// the deep repair cannot fix it either.
    pub dpkg_configure_fails: bool,
    /// installing the local package fails for a real reason.
    pub apt_install_deb_fails: bool,
    /// the index refresh exits non-zero. alone it models an unreachable
    /// third-party repository; with an unpopulated index, a real failure.
    pub apt_update_fails: bool,
    /// the SELinux boolean's state.
    ///
    /// `None` means unqueryable, which is **not** "off": the step concludes
    /// nothing and touches no policy.
    pub selinux_boolean: Option<bool>,
    /// is the cluster already initialised?
    ///
    /// a field of its own rather than the global path flag, which answers for
    /// **every** path: turning that on for the cluster would claim the home
    /// exists too.
    pub pg_cluster_initialized: bool,
    /// the modelled family, which decides **which catalogue** the package
    /// manager answers with.
    ///
    /// needed by the family-equivalence test: steps that do not go through the
    /// two boundaries must behave identically on both.
    pub family: OsFamily,
}

impl Default for MockConfig {
    fn default() -> Self {
        MockConfig {
            user_exists: false,
            path_exists: false,
            owner: OwnerId { uid: 0, gid: 0 },
            dir_empty: true,
            installed_packages: HashSet::new(),
            packages_without_candidate: HashSet::new(),
            virtual_packages: HashSet::new(),
            apt_index_populated: true,
            wk_version: None,
            service_enabled: false,
            service_active: false,
            service_start_fails: false,
            role_exists: false,
            db_exists: false,
            pg_databases_list: Vec::new(),
            source_state: OdooSourceState::Absent,
            git_clone_fail_times: 0,
            tarball_fails: false,
            network_failures_are_timeouts: false,
            venv_exists: false,
            venv_available: true,
            pg_declared_data_dir: None,
            run_as_user_fails_on: None,
            python_version: Some((3, 12)),
            requirements_content: None,
            db_initialized: false,
            real_fs: false,
            default_site_exists: false,
            default_site_kind: None,
            our_link_exists: false,
            ufw_available: false,
            ufw_active: false,
            existing_ufw_rules: HashSet::new(),
            nginx_test_ok: true,
            sudo_home: None,
            dpkg_broken: false,
            fix_broken_fails: false,
            dpkg_configure_fails: false,
            apt_install_deb_fails: false,
            apt_update_fails: false,
            selinux_boolean: Some(false),
            pg_cluster_initialized: false,
            family: OsFamily::Debian,
        }
    }
}

/// a shared handle to the operations log, inspectable after the step has taken
/// ownership of the mock.
pub type OpLog = Arc<Mutex<Vec<Op>>>;

pub struct MockSystemOps {
    log: OpLog,
    cfg: MockConfig,
    // the two multi-distro boundaries. fields and not separate objects
    // because the accessors return references: the mock must own them and
    // share their state.
    packages: MockPackageManager,
    distro: MockDistro,
    // service state with interior mutability, so the post-start check really
    // sees what start did.
    active: Cell<bool>,
    enabled: Cell<bool>,
    // clone attempt count, for the simulated initial failures.
    git_clone_calls: Cell<u32>,
    // the schema flips to present after the init.
    db_initialized: Cell<bool>,
}

/// the error the manager returns on an inconsistent package database, verbatim
/// from the one observed on the test VM.
fn unmet_dependencies(command: &str) -> StepError {
    StepError::CommandFailed {
        command: command.to_string(),
        status: "100".to_string(),
        stderr: "E: Unmet dependencies. Try 'apt --fix-broken install' with no packages (or \
                 specify a solution)."
            .to_string(),
    }
}

impl MockSystemOps {
    /// creates the mock and returns the log handle for assertions.
    pub fn new(cfg: MockConfig) -> (Self, OpLog) {
        let log: OpLog = Arc::new(Mutex::new(Vec::new()));
        (Self::with_log(cfg, Arc::clone(&log)), log)
    }

    /// creates the mock over a shared log, to check ordering between steps.
    pub fn with_log(cfg: MockConfig, log: OpLog) -> Self {
        let active = Cell::new(cfg.service_active);
        let enabled = Cell::new(cfg.service_enabled);
        let db_initialized = Cell::new(cfg.db_initialized);
        let dpkg_broken = Rc::new(Cell::new(cfg.dpkg_broken));
        let index_populated = Rc::new(Cell::new(cfg.apt_index_populated));
        let packages = MockPackageManager {
            log: Arc::clone(&log),
            cfg: cfg.clone(),
            dpkg_broken: Rc::clone(&dpkg_broken),
            index_populated: Rc::clone(&index_populated),
        };
        let distro = MockDistro {
            firewall: MockFirewall {
                log: Arc::clone(&log),
                cfg: cfg.clone(),
            },
            selinux: MockSelinux {
                log: Arc::clone(&log),
                cfg: cfg.clone(),
            },
            family: cfg.family,
            log: Arc::clone(&log),
            declared_pgdata: cfg.pg_declared_data_dir.clone(),
        };
        MockSystemOps {
            log,
            cfg,
            packages,
            distro,
            active,
            enabled,
            git_clone_calls: Cell::new(0),
            db_initialized,
        }
    }

    fn record(&self, op: Op) {
        if let Ok(mut entries) = self.log.lock() {
            entries.push(op);
        }
    }

    /// a simulated network failure: a timeout or a non-zero exit, per the
    /// configuration.
    fn simulated_network_failure(&self, command: &str, stderr: &str) -> StepError {
        if self.cfg.network_failures_are_timeouts {
            StepError::Timeout {
                command: command.to_string(),
                secs: 300,
            }
        } else {
            StepError::CommandFailed {
                command: command.to_string(),
                status: "1".to_string(),
                stderr: stderr.to_string(),
            }
        }
    }
}

/// the mock's package manager.
///
/// it **shares** the log and the mutable state with [`MockSystemOps`]: the
/// tests assert on one sequence of operations, and the real sequence
/// interleaves packaging commands with others. two separate logs would make
/// exactly the ordering unverifiable.
pub struct MockPackageManager {
    log: OpLog,
    cfg: MockConfig,
    dpkg_broken: Rc<Cell<bool>>,
    index_populated: Rc<Cell<bool>>,
}

impl MockPackageManager {
    fn record(&self, op: Op) {
        if let Ok(mut entries) = self.log.lock() {
            entries.push(op);
        }
    }
}

impl PackageManager for MockPackageManager {
    fn is_installed(&self, pkg: &str) -> bool {
        self.cfg.installed_packages.contains(pkg)
    }
    fn refresh_index(&self) -> Result<(), StepError> {
        self.record(Op::PkgRefreshIndex);
        if self.cfg.apt_update_fails {
            return Err(StepError::CommandFailed {
                command: "apt-get update".to_string(),
                status: "100".to_string(),
                stderr: "E: Some index files failed to download (simulato)".to_string(),
            });
        }
        // the refresh populates the index: from here queries answer.
        self.index_populated.set(true);
        Ok(())
    }
    /// answers like the real manager: the real candidate first, the virtual
    /// fallback second.
    ///
    /// without an index **no** query answers — the case that produced the field
    /// false positive, which the mock must reproduce or the defect would become
    /// invisible again.
    fn availability(&self, pkg: &str) -> Availability {
        if !self.index_populated.get() {
            return Availability::Absent;
        }
        if self.cfg.virtual_packages.contains(pkg) {
            return Availability::VirtualOnly;
        }
        if self.cfg.packages_without_candidate.contains(pkg) {
            return Availability::Absent;
        }
        Availability::Real
    }
    fn index_is_queryable(&self) -> bool {
        self.index_populated.get()
    }
    fn install(&self, pkgs: &[&str]) -> Result<(), StepError> {
        self.record(Op::PkgInstall(pkgs.iter().map(|s| s.to_string()).collect()));
        Ok(())
    }
    fn remove(&self, pkgs: &[&str]) -> Result<(), StepError> {
        self.record(Op::PkgRemove(pkgs.iter().map(|s| s.to_string()).collect()));
        // models A-RT-2: on a broken database the manager refuses to operate
        // until a repair puts it right.
        if self.dpkg_broken.get() {
            return Err(unmet_dependencies("apt-get purge"));
        }
        Ok(())
    }
    fn remove_orphans(&self) -> Result<(), StepError> {
        self.record(Op::PkgRemoveOrphans);
        Ok(())
    }
    fn try_repair(&self) -> Result<(), StepError> {
        self.record(Op::PkgRepair);
        if self.cfg.fix_broken_fails {
            return Err(unmet_dependencies("apt-get install -f"));
        }
        self.dpkg_broken.set(false);
        Ok(())
    }
    fn try_deep_repair(&self) -> Result<(), StepError> {
        self.record(Op::PkgDeepRepair);
        if self.cfg.dpkg_configure_fails {
            return Err(unmet_dependencies("dpkg --configure -a"));
        }
        self.dpkg_broken.set(false);
        Ok(())
    }
    fn install_local_file(&self, path: &Path) -> Result<(), StepError> {
        self.record(Op::PkgInstallLocalFile(path.to_path_buf()));
        if self.cfg.apt_install_deb_fails {
            return Err(StepError::CommandFailed {
                command: "apt-get install -y -- <deb>".to_string(),
                status: "100".to_string(),
                stderr: "impossibile installare il .deb (simulato)".to_string(),
            });
        }
        // the manager resolves the dependencies, leaving it consistent.
        self.dpkg_broken.set(false);
        Ok(())
    }
    /// the modelled family's command, as in production: a mock that always
    /// answered with one family's would make green a message that sends the
    /// other's users to a command that does not exist.
    fn local_package_name(&self, version: &str, suffix: &str) -> String {
        match self.cfg.family {
            OsFamily::Debian => {
                invok::packaging::apt::AptBackend.local_package_name(version, suffix)
            }
            OsFamily::Fedora => {
                invok::packaging::dnf::DnfBackend.local_package_name(version, suffix)
            }
        }
    }

    fn refresh_command(&self) -> &'static str {
        match self.cfg.family {
            OsFamily::Debian => invok::packaging::apt::AptBackend.refresh_command(),
            OsFamily::Fedora => invok::packaging::dnf::DnfBackend.refresh_command(),
        }
    }

    fn catalog(&self) -> PackageCatalog {
        // the **production** catalogue of the modelled family: the step tests
        // must see the names a real installation would.
        match self.cfg.family {
            OsFamily::Debian => invok::packaging::apt::AptBackend.catalog(),
            OsFamily::Fedora => invok::packaging::dnf::DnfBackend.catalog(),
        }
    }
}

/// the mock's firewall.
pub struct MockFirewall {
    log: OpLog,
    cfg: MockConfig,
}

impl MockFirewall {
    fn record(&self, op: Op) {
        if let Ok(mut entries) = self.log.lock() {
            entries.push(op);
        }
    }
}

impl Firewall for MockFirewall {
    /// the **modelled family's** tool name: always answering with one family's
    /// would make green a message that sends the other's users looking for a
    /// tool they do not have.
    fn name(&self) -> &'static str {
        match self.cfg.family {
            OsFamily::Debian => "ufw",
            OsFamily::Fedora => "firewalld",
        }
    }

    fn available(&self) -> bool {
        self.cfg.ufw_available
    }
    fn is_active(&self) -> bool {
        self.cfg.ufw_active
    }
    /// answers **like the real tool**: renders status-style output from the
    /// configured rules and queries it with the same function production uses.
    ///
    /// it used to be set membership — an ideal semantics the real command does
    /// not have, and the reason no test could notice A-V3-7.
    fn rule_exists(&self, rule: &str) -> Result<bool, StepError> {
        let mut status = String::from("Status: active\n\nTo   Action   From\n--   ------   ----\n");
        for existing in &self.cfg.existing_ufw_rules {
            status.push_str(&format!(
                "{existing}                   ALLOW       Anywhere\n"
            ));
        }
        Ok(invok::distro::ufw::rule_in_status(&status, rule))
    }
    fn allow(&self, rule: &str) -> Result<(), StepError> {
        self.record(Op::UfwAllow(rule.to_string()));
        Ok(())
    }
    fn delete(&self, rule: &str) -> Result<(), StepError> {
        self.record(Op::UfwDelete(rule.to_string()));
        Ok(())
    }
}

/// the mock's distribution conventions.
pub struct MockDistro {
    firewall: MockFirewall,
    selinux: MockSelinux,
    family: OsFamily,
    log: OpLog,
    /// the PGDATA the unit declares (A-MD-6); `None` means unknown.
    declared_pgdata: Option<PathBuf>,
}

impl MockDistro {
    fn record(&self, op: Op) {
        if let Ok(mut entries) = self.log.lock() {
            entries.push(op);
        }
    }
}

/// the mock's SELinux: present only on families that really have it.
pub struct MockSelinux {
    log: OpLog,
    cfg: MockConfig,
}

impl invok::distro::Selinux for MockSelinux {
    fn nginx_proxy_boolean(&self) -> &'static str {
        "httpd_can_network_connect"
    }
    fn is_enabled(&self, _boolean: &str) -> Option<bool> {
        self.cfg.selinux_boolean
    }
    fn set(&self, boolean: &str, value: bool) -> Result<(), StepError> {
        if let Ok(mut entries) = self.log.lock() {
            entries.push(Op::SetSelinuxBoolean {
                boolean: boolean.to_string(),
                value,
            });
        }
        Ok(())
    }
}

impl Distro for MockDistro {
    fn firewall(&self) -> &dyn Firewall {
        &self.firewall
    }

    /// follows the family: where SELinux is not in use, a step that found it
    /// anyway would mutate a policy the system does not have.
    fn selinux(&self) -> Option<&dyn invok::distro::Selinux> {
        match self.family {
            OsFamily::Debian => None,
            OsFamily::Fedora => Some(&self.selinux),
        }
    }

    /// follows the modelled family: always answering `Some` would initialise a
    /// cluster where the package already creates one.
    /// the **production** layout of the modelled family: a fake one would make
    /// green tests that in the field write the vhost where nginx never
    /// reads.
    fn nginx_layout(&self) -> invok::distro::NginxLayout {
        match self.family {
            OsFamily::Debian => invok::distro::debian::Debian::new().nginx_layout(),
            OsFamily::Fedora => invok::distro::fedora::Fedora::new().nginx_layout(),
        }
    }

    /// the PGDATA the unit would declare (A-MD-6). `None` — unknown — is the
    /// default: most tests have nothing to do with this question and must not
    /// answer it by accident.
    fn declared_postgres_data_dir(&self) -> Option<std::path::PathBuf> {
        self.declared_pgdata.clone()
    }

    fn postgres_data_dir(&self) -> Option<std::path::PathBuf> {
        match self.family {
            OsFamily::Debian => None,
            OsFamily::Fedora => Some(std::path::PathBuf::from(
                invok::distro::fedora::POSTGRES_DATA_DIR,
            )),
        }
    }

    fn init_postgres_cluster(&self) -> Result<(), StepError> {
        self.record(Op::InitPostgresCluster);
        Ok(())
    }
}

impl SystemOps for MockSystemOps {
    fn user_exists(&self, _user: &str) -> bool {
        self.cfg.user_exists
    }
    fn path_exists(&self, path: &Path) -> bool {
        // the cluster marker has its own answer: the global path flag would
        // tie two unrelated questions together.
        if path.ends_with("PG_VERSION") {
            return self.cfg.pg_cluster_initialized;
        }
        if self.cfg.real_fs {
            path.exists()
        } else {
            self.cfg.path_exists
        }
    }
    fn owner_of(&self, _path: &Path) -> Result<OwnerId, StepError> {
        Ok(self.cfg.owner)
    }
    fn dir_is_empty(&self, _path: &Path) -> Result<bool, StepError> {
        Ok(self.cfg.dir_empty)
    }
    fn create_user(&self, spec: &UserSpec) -> Result<(), StepError> {
        self.record(Op::CreateUser(spec.clone()));
        Ok(())
    }
    fn delete_user(&self, user: &str) -> Result<(), StepError> {
        self.record(Op::DeleteUser(user.to_string()));
        Ok(())
    }
    fn delete_group(&self, group: &str) -> Result<(), StepError> {
        self.record(Op::DeleteGroup(group.to_string()));
        Ok(())
    }
    fn chown_named(&self, path: &Path, owner: &str, group: &str) -> Result<(), StepError> {
        self.record(Op::ChownNamed {
            path: path.to_path_buf(),
            owner: owner.to_string(),
            group: group.to_string(),
        });
        Ok(())
    }
    fn chown_numeric(&self, path: &Path, id: OwnerId) -> Result<(), StepError> {
        self.record(Op::ChownNumeric {
            path: path.to_path_buf(),
            id,
        });
        Ok(())
    }
    fn chmod(&self, path: &Path, mode: u32) -> Result<(), StepError> {
        self.record(Op::Chmod {
            path: path.to_path_buf(),
            mode,
        });
        if self.cfg.real_fs {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
                .map_err(|e| StepError::io(path, e))?;
        }
        Ok(())
    }
    fn mkdir(&self, path: &Path) -> Result<(), StepError> {
        self.record(Op::Mkdir(path.to_path_buf()));
        Ok(())
    }
    fn rmdir(&self, path: &Path) -> Result<(), StepError> {
        self.record(Op::Rmdir(path.to_path_buf()));
        Ok(())
    }

    fn packages(&self) -> &dyn PackageManager {
        &self.packages
    }

    fn distro(&self) -> &dyn Distro {
        &self.distro
    }

    fn wkhtmltopdf_version(&self) -> Option<String> {
        self.cfg.wk_version.clone()
    }

    fn service_is_enabled(&self, _service: &str) -> bool {
        self.enabled.get()
    }
    fn service_is_active(&self, _service: &str) -> bool {
        self.active.get()
    }
    fn service_enable(&self, service: &str) -> Result<(), StepError> {
        self.enabled.set(true);
        self.record(Op::ServiceEnable(service.to_string()));
        Ok(())
    }
    fn service_disable(&self, service: &str) -> Result<(), StepError> {
        self.enabled.set(false);
        self.record(Op::ServiceDisable(service.to_string()));
        Ok(())
    }
    fn service_start(&self, service: &str) -> Result<(), StepError> {
        self.active.set(!self.cfg.service_start_fails);
        self.record(Op::ServiceStart(service.to_string()));
        Ok(())
    }
    fn service_stop(&self, service: &str) -> Result<(), StepError> {
        self.active.set(false);
        self.record(Op::ServiceStop(service.to_string()));
        Ok(())
    }
    fn service_restart(&self, service: &str) -> Result<(), StepError> {
        self.active.set(!self.cfg.service_start_fails);
        self.record(Op::ServiceRestart(service.to_string()));
        Ok(())
    }
    fn service_reload(&self, service: &str) -> Result<(), StepError> {
        self.record(Op::ServiceReload(service.to_string()));
        Ok(())
    }
    fn daemon_reload(&self) -> Result<(), StepError> {
        self.record(Op::DaemonReload);
        Ok(())
    }
    fn create_symlink(&self, src: &Path, link: &Path) -> Result<(), StepError> {
        self.record(Op::CreateSymlink {
            src: src.to_path_buf(),
            link: link.to_path_buf(),
        });
        Ok(())
    }
    fn remove_symlink(&self, link: &Path) -> Result<(), StepError> {
        self.record(Op::RemoveSymlink(link.to_path_buf()));
        Ok(())
    }
    fn symlink_exists(&self, link: &Path) -> bool {
        if link.ends_with("default") {
            self.cfg.default_site_exists
        } else {
            self.cfg.our_link_exists
        }
    }
    fn path_kind(&self, path: &Path) -> PathKind {
        if !path.ends_with("default") {
            return if self.cfg.our_link_exists {
                PathKind::Symlink {
                    target: PathBuf::from("/etc/nginx/sites-available/odoo18"),
                }
            } else {
                PathKind::Absent
            };
        }
        if let Some(kind) = &self.cfg.default_site_kind {
            return kind.clone();
        }
        if self.cfg.default_site_exists {
            PathKind::Symlink {
                target: PathBuf::from("/etc/nginx/sites-available/default"),
            }
        } else {
            PathKind::Absent
        }
    }
    fn nginx_test(&self) -> bool {
        self.cfg.nginx_test_ok
    }

    fn pg_role_exists(&self, _role: &str) -> Result<bool, StepError> {
        Ok(self.cfg.role_exists)
    }
    fn pg_db_exists(&self, _db: &str) -> Result<bool, StepError> {
        Ok(self.cfg.db_exists)
    }
    fn pg_create_role(&self, role: &str, password: Option<&str>) -> Result<(), StepError> {
        // records ONLY that a password was present, never its value.
        self.record(Op::PgCreateRole {
            role: role.to_string(),
            has_password: password.is_some(),
        });
        Ok(())
    }
    fn pg_drop_role(&self, role: &str) -> Result<(), StepError> {
        self.record(Op::PgDropRole(role.to_string()));
        Ok(())
    }
    fn createdb(&self, owner: &str, db: &str) -> Result<(), StepError> {
        self.record(Op::CreateDb {
            owner: owner.to_string(),
            db: db.to_string(),
        });
        Ok(())
    }
    fn dropdb(&self, db: &str) -> Result<(), StepError> {
        self.record(Op::DropDb(db.to_string()));
        Ok(())
    }
    fn pg_list_databases(&self) -> Result<Vec<String>, StepError> {
        Ok(self.cfg.pg_databases_list.clone())
    }

    fn run_as_user(&self, user: &str, program: &str, args: &[&str]) -> Result<(), StepError> {
        self.record(Op::RunAsUser {
            user: user.to_string(),
            program: program.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
        });
        // fails ONE precise invocation, selected by a fragment of its
        // arguments: needed to exercise what happens *after* the failure, which
        // is only observable if the command really fails through the step.
        if let Some(frammento) = &self.cfg.run_as_user_fails_on {
            if args.iter().any(|a| a.contains(frammento.as_str())) {
                return Err(StepError::CommandFailed {
                    command: format!("{program} {}", args.join(" ")),
                    status: "1".to_string(),
                    stderr: "error: subprocess-exited-with-error\n× Building wheel for gevent \
                             (pyproject.toml) did not run successfully."
                        .to_string(),
                });
            }
        }
        Ok(())
    }
    fn mkdir_p_as_user(&self, user: &str, path: &Path) -> Result<(), StepError> {
        self.record(Op::MkdirAsUser {
            user: user.to_string(),
            path: path.to_path_buf(),
        });
        Ok(())
    }
    fn remove_dir_all(&self, path: &Path) -> Result<(), StepError> {
        self.record(Op::RemoveDirAll(path.to_path_buf()));
        Ok(())
    }
    fn detect_odoo_source(
        &self,
        _user: &str,
        _target: &Path,
    ) -> Result<OdooSourceState, StepError> {
        Ok(self.cfg.source_state.clone())
    }
    fn git_clone(
        &self,
        _user: &str,
        _url: &str,
        branch: &str,
        depth: u32,
        target: &Path,
    ) -> Result<(), StepError> {
        let n = self.git_clone_calls.get();
        self.git_clone_calls.set(n + 1);
        self.record(Op::GitClone {
            target: target.to_path_buf(),
            branch: branch.to_string(),
            depth,
        });
        if n < self.cfg.git_clone_fail_times {
            Err(self.simulated_network_failure("git clone", "fallimento clone simulato"))
        } else {
            Ok(())
        }
    }
    fn tarball_install(&self, _user: &str, _url: &str, target: &Path) -> Result<(), StepError> {
        self.record(Op::TarballInstall {
            target: target.to_path_buf(),
        });
        if self.cfg.tarball_fails {
            if self.cfg.network_failures_are_timeouts {
                return Err(self.simulated_network_failure("wget tarball", ""));
            }
            Err(StepError::Precondition(
                "tarball fallito (simulato)".to_string(),
            ))
        } else {
            Ok(())
        }
    }
    fn venv_python_exists(&self, _venv: &Path) -> bool {
        self.cfg.venv_exists
    }
    fn python_venv_available(&self, _python: &str) -> bool {
        self.cfg.venv_available
    }
    fn python_version(&self, python: &str) -> Option<(u32, u32)> {
        self.record(Op::PythonVersion(python.to_string()));
        self.cfg.python_version
    }
    fn create_venv(&self, _user: &str, _python: &str, venv: &Path) -> Result<(), StepError> {
        self.record(Op::CreateVenv {
            python: _python.to_string(),
            venv: venv.to_path_buf(),
        });
        Ok(())
    }
    fn read_to_string(&self, path: &Path) -> Result<String, StepError> {
        if self.cfg.real_fs {
            return std::fs::read_to_string(path).map_err(|e| StepError::io(path, e));
        }
        self.cfg
            .requirements_content
            .clone()
            .ok_or_else(|| StepError::io(path, std::io::Error::from(std::io::ErrorKind::NotFound)))
    }

    fn write_private_file(&self, path: &Path, content: &str) -> Result<(), StepError> {
        self.record(Op::WritePrivateFile(path.to_path_buf()));
        if self.cfg.real_fs {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(path)
                .map_err(|e| StepError::io(path, e))?;
            f.write_all(content.as_bytes())
                .map_err(|e| StepError::io(path, e))?;
        }
        Ok(())
    }

    fn create_private_file(&self, path: &Path, content: &str) -> Result<(), StepError> {
        self.record(Op::CreatePrivateFile(path.to_path_buf()));
        if self.cfg.real_fs {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            // the same guarantees as the real one.
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .custom_flags(nix::libc::O_NOFOLLOW)
                .mode(0o600)
                .open(path)
                .map_err(|e| StepError::io(path, e))?;
            f.write_all(content.as_bytes())
                .map_err(|e| StepError::io(path, e))?;
        }
        Ok(())
    }
    fn move_file(&self, src: &Path, dst: &Path) -> Result<(), StepError> {
        self.record(Op::MoveFile {
            src: src.to_path_buf(),
            dst: dst.to_path_buf(),
        });
        if self.cfg.real_fs {
            std::fs::rename(src, dst).map_err(|e| StepError::io(dst, e))?;
        }
        Ok(())
    }
    fn copy_file(&self, src: &Path, dst: &Path) -> Result<(), StepError> {
        self.record(Op::CopyFile {
            src: src.to_path_buf(),
            dst: dst.to_path_buf(),
        });
        if self.cfg.real_fs {
            std::fs::copy(src, dst).map_err(|e| StepError::io(dst, e))?;
        }
        Ok(())
    }
    fn remove_file(&self, path: &Path) -> Result<(), StepError> {
        self.record(Op::RemoveFile(path.to_path_buf()));
        if self.cfg.real_fs {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(StepError::io(path, e)),
            }
        }
        Ok(())
    }
    fn pg_db_initialized(&self, _db: &str) -> Result<bool, StepError> {
        Ok(self.db_initialized.get())
    }
    fn odoo_init_base(
        &self,
        _user: &str,
        _python: &Path,
        _odoo_bin: &Path,
        conf: &Path,
        db: &str,
    ) -> Result<(), StepError> {
        self.record(Op::OdooInitBase {
            conf: conf.to_path_buf(),
            db: db.to_string(),
        });
        self.db_initialized.set(true);
        Ok(())
    }
    fn getent_home(&self, _user: &str) -> Result<Option<String>, StepError> {
        Ok(self.cfg.sudo_home.clone())
    }
    fn chown_to_user(&self, path: &Path, user: &str) -> Result<(), StepError> {
        self.record(Op::ChownToUser {
            path: path.to_path_buf(),
            user: user.to_string(),
        });
        Ok(())
    }
    fn append_line(&self, path: &Path, line: &str) -> Result<(), StepError> {
        self.record(Op::AppendLine(path.to_path_buf()));
        if self.cfg.real_fs {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|e| StepError::io(path, e))?;
            writeln!(f, "{line}").map_err(|e| StepError::io(path, e))?;
        }
        Ok(())
    }
}

/// the mock downloader: writes known bytes so the tests compute a real
/// SHA-256, and records the download.
pub struct MockDownloader {
    bytes: Vec<u8>,
    log: OpLog,
}

impl MockDownloader {
    pub fn new(bytes: Vec<u8>, log: OpLog) -> Self {
        MockDownloader { bytes, log }
    }
}

impl Downloader for MockDownloader {
    fn download(&self, url: &str, dest: &Path) -> Result<(), StepError> {
        std::fs::write(dest, &self.bytes).map_err(|e| StepError::io(dest, e))?;
        if let Ok(mut entries) = self.log.lock() {
            entries.push(Op::Download {
                url: url.to_string(),
                dest: dest.to_path_buf(),
            });
        }
        Ok(())
    }
}

/// a snapshot of the log, for assertions.
pub fn ops_of(log: &OpLog) -> Vec<Op> {
    log.lock().expect("lock").clone()
}

/// a progress reporter that records its events as strings.
pub type EventLog = Arc<Mutex<Vec<String>>>;

pub struct RecordingReporter {
    events: EventLog,
}

impl RecordingReporter {
    pub fn new() -> (Self, EventLog) {
        let events: EventLog = Arc::new(Mutex::new(Vec::new()));
        (
            RecordingReporter {
                events: Arc::clone(&events),
            },
            events,
        )
    }
    fn push(&self, event: String) {
        if let Ok(mut v) = self.events.lock() {
            v.push(event);
        }
    }
}

impl ProgressReporter for RecordingReporter {
    fn step_start(&self, name: &str, _i: usize, _t: usize) {
        self.push(format!("start:{name}"));
    }
    fn step_done(&self, name: &str) {
        self.push(format!("done:{name}"));
    }
    fn step_failed(&self, name: &str) {
        self.push(format!("failed:{name}"));
    }
    fn rollback_start(&self, _total: usize) {
        self.push("rollback".to_string());
    }
    fn undo_start(&self, name: &str) {
        self.push(format!("undo:{name}"));
    }
    fn undo_done(&self, name: &str) {
        self.push(format!("undo-done:{name}"));
    }
}

pub fn events_of(log: &EventLog) -> Vec<String> {
    log.lock().expect("lock").clone()
}
