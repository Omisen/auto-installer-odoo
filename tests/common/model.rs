//! `SystemModel`: a **stateful, coherent** model of the system, for the
//! end-to-end rollback tests.
//!
//! unlike the mock that records operations, here every mutation updates shared
//! state and every undo restores it, so "the system is back to pristine" is
//! literally checkable.
//!
//! all the steps of a sequence share the **same** model, so one step's
//! mutations are visible to another's undo.

#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use invok::distro::{Distro, Firewall};
use invok::error::StepError;
use invok::packaging::{Availability, PackageCatalog, PackageManager};
use invok::system_ops::{OdooSourceState, OwnerId, PathKind, SystemOps, UserSpec};

/// the modelled system state; comparable to check start against end.
/// the mode a directory nobody chmodded is assumed to have: traversable, so a
/// chain does not have to widen a root in order to run.
pub const DEFAULT_MODE: u32 = 0o755;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelState {
    pub packages: HashSet<String>,
    pub users: HashSet<String>,
    pub groups: HashSet<String>,
    pub svc_enabled: HashSet<String>,
    pub svc_active: HashSet<String>,
    pub pg_roles: HashSet<String>,
    pub pg_dbs: HashSet<String>,
    pub pg_initialized: HashSet<String>,
    pub paths: HashSet<PathBuf>,
    pub symlinks: HashSet<PathBuf>,
    pub ufw_rules: HashSet<String>,
    /// the nginx config **loaded in memory** by the running process: the sites
    /// enabled at the last start or reload. `None` when nginx is not running.
    ///
    /// models the difference between the config *on disk* and the config
    /// *being served*: a running nginx keeps serving what it loaded. without
    /// that distinction a rollback can look complete while the customer's
    /// service is still serving ours.
    pub nginx_loaded_sites: Option<HashSet<PathBuf>>,
    /// the package database left inconsistent: the manager refuses to operate
    /// until a repair fixes it.
    pub dpkg_broken: bool,
    /// system dependencies a local package needs and that are missing, as on a
    /// minimal VM: installed by whoever resolves dependencies.
    pub pending_deps: HashSet<String>,
    pub file_contents: HashMap<PathBuf, String>,
    /// permission bits, for the paths whose mode is **not** the default
    /// (`A-V6-9`).
    ///
    /// only the deviations, deliberately: absent means `DEFAULT_MODE`, and a
    /// `chmod` back to it removes the entry. that keeps the invariant the whole
    /// model rests on — two states compare equal **iff** every path's effective
    /// mode is the same — so a fixture never has to spell out the modes of the
    /// directories it did not touch, and a mode restored to what it was becomes
    /// indistinguishable from one that was never changed. which is the point.
    pub modes: HashMap<PathBuf, u32>,
    // environment, which does not mutate and does not enter the comparison.
    pub ufw_available: bool,
    pub ufw_active: bool,
    pub wk_version: Option<String>,
    pub sudo_home: Option<String>,
    /// package names that do not exist on this "release" (A5.1); empty means
    /// everything is installable.
    pub packages_without_candidate: HashSet<String>,
    /// **virtual** names: no real candidate, but resolvable (A5.1-bis).
    pub virtual_packages: HashSet<String>,
    /// the index starts **stale**, as on a machine never refreshed.
    ///
    /// only the *initial* condition: the index's state lives outside
    /// `ModelState`, because refreshing it must not show up in the start/end
    /// comparison — a fresh index is a cache, not an artifact to undo.
    pub apt_index_stale: bool,
}

/// a shared handle to the model.
#[derive(Clone)]
pub struct SystemModel {
    state: Arc<Mutex<ModelState>>,
    /// the package index: shared between handles, so one step's refresh is
    /// visible to the others, but **outside** the compared state.
    apt_index_populated: Arc<Mutex<bool>>,
    packages: ModelPackages,
    distro: ModelDistro,
}

impl SystemModel {
    pub fn new(state: ModelState) -> Self {
        let apt_index_populated = Arc::new(Mutex::new(!state.apt_index_stale));
        Self::from_parts(Arc::new(Mutex::new(state)), apt_index_populated)
    }
    /// another handle onto the same state, for another step.
    pub fn handle(&self) -> SystemModel {
        Self::from_parts(
            Arc::clone(&self.state),
            Arc::clone(&self.apt_index_populated),
        )
    }
    fn from_parts(state: Arc<Mutex<ModelState>>, apt_index_populated: Arc<Mutex<bool>>) -> Self {
        SystemModel {
            packages: ModelPackages {
                state: Arc::clone(&state),
                apt_index_populated: Arc::clone(&apt_index_populated),
            },
            distro: ModelDistro {
                firewall: ModelFirewall {
                    state: Arc::clone(&state),
                },
            },
            state,
            apt_index_populated,
        }
    }
    /// a snapshot of the current state, for the start/end comparison.
    pub fn snapshot(&self) -> ModelState {
        self.state.lock().expect("lock").clone()
    }
    /// mutates the state from outside, for test steps that simulate collateral
    /// damage.
    pub fn mutate(&self, f: impl FnOnce(&mut ModelState)) {
        f(&mut self.state.lock().expect("lock"));
    }
    pub fn boxed(&self) -> Box<dyn SystemOps> {
        Box::new(self.handle())
    }
}

fn under(entry: &Path, dir: &Path) -> bool {
    entry != dir && entry.starts_with(dir)
}

/// the error the manager returns on an inconsistent package database.
fn unmet_dependencies() -> StepError {
    StepError::CommandFailed {
        command: "apt-get".to_string(),
        status: "100".to_string(),
        stderr: "E: Unmet dependencies. Try 'apt --fix-broken install' with no packages (or \
                 specify a solution)."
            .to_string(),
    }
}

/// the enabled-sites directory.
pub const SITES_ENABLED: &str = "/etc/nginx/sites-enabled";

/// the sites currently enabled **on disk**.
fn enabled_sites(state: &ModelState) -> HashSet<PathBuf> {
    let dir = Path::new(SITES_ENABLED);
    state
        .symlinks
        .iter()
        .filter(|l| under(l, dir))
        .cloned()
        .collect()
}

/// the model's package manager: shares the state of the [`SystemModel`] that
/// owns it, so one step's install is visible to another's undo — the whole
/// point of this model.
#[derive(Clone)]
pub struct ModelPackages {
    state: Arc<Mutex<ModelState>>,
    apt_index_populated: Arc<Mutex<bool>>,
}

impl PackageManager for ModelPackages {
    fn is_transient_failure(&self, stderr: &str) -> bool {
        invok::packaging::apt::is_transient_fetch_failure(stderr)
    }

    fn is_installed(&self, pkg: &str) -> bool {
        self.state.lock().expect("l").packages.contains(pkg)
    }
    fn availability(&self, pkg: &str) -> Availability {
        if !self.index_is_queryable() {
            return Availability::Absent;
        }
        let s = self.state.lock().expect("l");
        if s.virtual_packages.contains(pkg) {
            Availability::VirtualOnly
        } else if s.packages_without_candidate.contains(pkg) {
            Availability::Absent
        } else {
            Availability::Real
        }
    }
    fn index_is_queryable(&self) -> bool {
        *self.apt_index_populated.lock().expect("l")
    }
    fn refresh_index(&self) -> Result<(), StepError> {
        *self.apt_index_populated.lock().expect("l") = true;
        Ok(())
    }
    fn install(&self, pkgs: &[&str]) -> Result<(), StepError> {
        let mut s = self.state.lock().expect("l");
        for p in pkgs {
            s.packages.insert(p.to_string());
        }
        Ok(())
    }
    fn remove(&self, pkgs: &[&str]) -> Result<(), StepError> {
        let mut s = self.state.lock().expect("l");
        // A-RT-2: on a broken package database the manager refuses to operate,
        // which is what left the whole delta installed on the test VM.
        if s.dpkg_broken {
            return Err(unmet_dependencies());
        }
        for p in pkgs {
            s.packages.remove(*p);
            if *p == "wkhtmltox" {
                s.wk_version = None;
            }
        }
        Ok(())
    }
    fn remove_orphans(&self) -> Result<(), StepError> {
        Ok(())
    }
    fn try_repair(&self) -> Result<(), StepError> {
        let mut s = self.state.lock().expect("l");
        // installs the missing dependencies and configures what was halfway.
        let deps = std::mem::take(&mut s.pending_deps);
        s.packages.extend(deps);
        s.dpkg_broken = false;
        Ok(())
    }
    fn try_deep_repair(&self) -> Result<(), StepError> {
        self.state.lock().expect("l").dpkg_broken = false;
        Ok(())
    }
    fn install_local_file(&self, _path: &Path) -> Result<(), StepError> {
        let mut s = self.state.lock().expect("l");
        if s.dpkg_broken {
            return Err(unmet_dependencies());
        }
        // the manager resolves the local package's dependencies in one go, so
        // it ends up configured and the database stays consistent.
        s.packages.insert("wkhtmltox".to_string());
        let deps = std::mem::take(&mut s.pending_deps);
        s.packages.extend(deps);
        s.wk_version = Some("0.12.6.1".to_string());
        Ok(())
    }
    fn local_package_name(&self, version: &str, suffix: &str) -> String {
        invok::packaging::apt::AptBackend.local_package_name(version, suffix)
    }

    fn refresh_command(&self) -> &'static str {
        invok::packaging::apt::AptBackend.refresh_command()
    }

    fn catalog(&self) -> PackageCatalog {
        invok::packaging::apt::AptBackend.catalog()
    }
}

/// the model's firewall.
#[derive(Clone)]
pub struct ModelFirewall {
    state: Arc<Mutex<ModelState>>,
}

impl Firewall for ModelFirewall {
    fn name(&self) -> &'static str {
        "ufw"
    }

    fn available(&self) -> bool {
        self.state.lock().expect("l").ufw_available
    }
    fn is_active(&self) -> bool {
        self.state.lock().expect("l").ufw_active
    }
    fn rule_exists(&self, rule: &str) -> Result<bool, StepError> {
        Ok(self.state.lock().expect("l").ufw_rules.contains(rule))
    }
    fn allow(&self, rule: &str) -> Result<(), StepError> {
        self.state
            .lock()
            .expect("l")
            .ufw_rules
            .insert(rule.to_string());
        Ok(())
    }
    fn delete(&self, rule: &str) -> Result<(), StepError> {
        self.state.lock().expect("l").ufw_rules.remove(rule);
        Ok(())
    }
}

/// the model's distribution conventions.
#[derive(Clone)]
pub struct ModelDistro {
    firewall: ModelFirewall,
}

impl Distro for ModelDistro {
    fn firewall(&self) -> &dyn Firewall {
        &self.firewall
    }

    /// the model is a Debian machine: the package creates the cluster.
    /// the model is a Debian machine: no SELinux.
    fn selinux(&self) -> Option<&dyn invok::distro::Selinux> {
        None
    }

    fn nginx_layout(&self) -> invok::distro::NginxLayout {
        invok::distro::debian::Debian::new().nginx_layout()
    }

    fn postgres_data_dir(&self) -> Option<PathBuf> {
        None
    }

    fn init_postgres_cluster(&self) -> Result<(), StepError> {
        Ok(())
    }
}

impl SystemOps for SystemModel {
    // --- queries --------------------------------------------------------------
    fn user_exists(&self, user: &str) -> bool {
        self.state.lock().expect("l").users.contains(user)
    }
    fn path_exists(&self, path: &Path) -> bool {
        let s = self.state.lock().expect("l");
        s.paths.contains(path) || s.symlinks.contains(path)
    }
    fn owner_of(&self, _path: &Path) -> Result<OwnerId, StepError> {
        Ok(OwnerId { uid: 0, gid: 0 })
    }
    fn mode_of(&self, path: &Path) -> Result<u32, StepError> {
        Ok(self
            .state
            .lock()
            .expect("l")
            .modes
            .get(path)
            .copied()
            .unwrap_or(DEFAULT_MODE))
    }
    fn dir_is_empty(&self, path: &Path) -> Result<bool, StepError> {
        let s = self.state.lock().expect("l");
        let has_child =
            s.paths.iter().any(|e| under(e, path)) || s.symlinks.iter().any(|e| under(e, path));
        Ok(!has_child)
    }
    fn packages(&self) -> &dyn PackageManager {
        &self.packages
    }
    fn distro(&self) -> &dyn Distro {
        &self.distro
    }
    fn wkhtmltopdf_version(&self) -> Option<String> {
        self.state.lock().expect("l").wk_version.clone()
    }
    fn service_is_enabled(&self, service: &str) -> bool {
        self.state.lock().expect("l").svc_enabled.contains(service)
    }
    fn service_is_active(&self, service: &str) -> bool {
        self.state.lock().expect("l").svc_active.contains(service)
    }
    fn pg_role_exists(&self, role: &str) -> Result<bool, StepError> {
        Ok(self.state.lock().expect("l").pg_roles.contains(role))
    }
    fn pg_db_exists(&self, db: &str) -> Result<bool, StepError> {
        Ok(self.state.lock().expect("l").pg_dbs.contains(db))
    }
    fn pg_db_initialized(&self, db: &str) -> Result<bool, StepError> {
        Ok(self.state.lock().expect("l").pg_initialized.contains(db))
    }
    fn pg_list_databases(&self) -> Result<Vec<String>, StepError> {
        Ok(self
            .state
            .lock()
            .expect("l")
            .pg_dbs
            .iter()
            .cloned()
            .collect())
    }
    fn symlink_exists(&self, link: &Path) -> bool {
        self.state.lock().expect("l").symlinks.contains(link)
    }
    fn path_kind(&self, path: &Path) -> PathKind {
        let st = self.state.lock().expect("l");
        if st.symlinks.contains(path) {
            // the model does not track symlink targets, so the default site
            // gets the standard one, as a real machine has.
            return PathKind::Symlink {
                target: std::path::PathBuf::from("/etc/nginx/sites-available/default"),
            };
        }
        if st.file_contents.contains_key(path) {
            return PathKind::RegularFile;
        }
        if st.paths.contains(path) {
            return PathKind::Other;
        }
        PathKind::Absent
    }
    fn nginx_test(&self) -> bool {
        true
    }
    fn detect_odoo_source(&self, _user: &str, target: &Path) -> Result<OdooSourceState, StepError> {
        let s = self.state.lock().expect("l");
        if s.paths.contains(&target.join(".git")) {
            return Ok(OdooSourceState::GitRepo {
                branch: "18.0".to_string(),
            });
        }
        if s.paths.contains(target) {
            if s.paths.contains(&target.join("odoo-bin")) {
                return Ok(OdooSourceState::TarballPresent);
            }
            return Ok(OdooSourceState::InvalidDir);
        }
        Ok(OdooSourceState::Absent)
    }
    fn venv_python_exists(&self, venv: &Path) -> bool {
        self.state
            .lock()
            .expect("l")
            .paths
            .contains(&venv.join("bin").join("python3"))
    }
    fn python_venv_available(&self, _python: &str) -> bool {
        true
    }
    /// the model simulates the *system*, not the interpreter: a Python covered
    /// by Odoo's pins, so the end-to-end tests never cross A-MD-7's
    /// diagnosis.
    fn python_version(&self, _python: &str) -> Option<(u32, u32)> {
        Some((3, 12))
    }
    fn read_to_string(&self, path: &Path) -> Result<String, StepError> {
        self.state
            .lock()
            .expect("l")
            .file_contents
            .get(path)
            .cloned()
            .ok_or_else(|| StepError::io(path, std::io::Error::from(std::io::ErrorKind::NotFound)))
    }
    fn getent_home(&self, _user: &str) -> Result<Option<String>, StepError> {
        Ok(self.state.lock().expect("l").sudo_home.clone())
    }

    // --- mutations, with symmetric undos --------------------------------------
    fn create_user(&self, spec: &UserSpec) -> Result<(), StepError> {
        let mut s = self.state.lock().expect("l");
        s.users.insert(spec.name.clone());
        if spec.user_group {
            s.groups.insert(spec.name.clone());
        }
        Ok(())
    }
    fn delete_user(&self, user: &str) -> Result<(), StepError> {
        self.state.lock().expect("l").users.remove(user);
        Ok(())
    }
    fn delete_group(&self, group: &str) -> Result<(), StepError> {
        self.state.lock().expect("l").groups.remove(group);
        Ok(())
    }
    fn chown_named(&self, _p: &Path, _o: &str, _g: &str) -> Result<(), StepError> {
        Ok(())
    }
    fn chown_numeric(&self, _p: &Path, _id: OwnerId) -> Result<(), StepError> {
        Ok(())
    }
    fn chown_to_user(&self, _p: &Path, _u: &str) -> Result<(), StepError> {
        Ok(())
    }
    fn chmod(&self, p: &Path, m: u32) -> Result<(), StepError> {
        let mut s = self.state.lock().expect("l");
        if m == DEFAULT_MODE {
            s.modes.remove(p);
        } else {
            s.modes.insert(p.to_path_buf(), m);
        }
        Ok(())
    }
    fn mkdir(&self, path: &Path) -> Result<(), StepError> {
        self.state
            .lock()
            .expect("l")
            .paths
            .insert(path.to_path_buf());
        Ok(())
    }
    fn mkdir_p_as_user(&self, _user: &str, path: &Path) -> Result<(), StepError> {
        self.state
            .lock()
            .expect("l")
            .paths
            .insert(path.to_path_buf());
        Ok(())
    }
    fn rmdir(&self, path: &Path) -> Result<(), StepError> {
        let mut s = self.state.lock().expect("l");
        s.paths.remove(path);
        s.modes.remove(path);
        Ok(())
    }
    fn remove_dir_all(&self, path: &Path) -> Result<(), StepError> {
        let mut s = self.state.lock().expect("l");
        s.paths.retain(|e| e != path && !e.starts_with(path));
        s.symlinks.retain(|e| e != path && !e.starts_with(path));
        s.file_contents
            .retain(|k, _| k != path && !k.starts_with(path));
        s.modes.retain(|k, _| k != path && !k.starts_with(path));
        Ok(())
    }
    fn create_symlink(&self, _src: &Path, link: &Path) -> Result<(), StepError> {
        self.state
            .lock()
            .expect("l")
            .symlinks
            .insert(link.to_path_buf());
        Ok(())
    }
    fn remove_symlink(&self, link: &Path) -> Result<(), StepError> {
        let mut s = self.state.lock().expect("l");
        s.symlinks.remove(link);
        s.modes.remove(link);
        Ok(())
    }
    fn write_private_file(&self, path: &Path, content: &str) -> Result<(), StepError> {
        let mut s = self.state.lock().expect("l");
        s.paths.insert(path.to_path_buf());
        s.file_contents
            .insert(path.to_path_buf(), content.to_string());
        Ok(())
    }
    fn create_private_file(&self, path: &Path, content: &str) -> Result<(), StepError> {
        // models `O_EXCL`: creating over an existing path is an error.
        let mut s = self.state.lock().expect("l");
        if s.paths.contains(path) || s.symlinks.contains(path) {
            return Err(StepError::io(
                path,
                std::io::Error::from(std::io::ErrorKind::AlreadyExists),
            ));
        }
        s.paths.insert(path.to_path_buf());
        s.file_contents
            .insert(path.to_path_buf(), content.to_string());
        Ok(())
    }
    fn move_file(&self, src: &Path, dst: &Path) -> Result<(), StepError> {
        let mut s = self.state.lock().expect("l");
        let content = s.file_contents.remove(src);
        s.paths.remove(src);
        s.symlinks.remove(src);
        if let Some(mode) = s.modes.remove(src) {
            s.modes.insert(dst.to_path_buf(), mode);
        }
        s.paths.insert(dst.to_path_buf());
        if let Some(c) = content {
            s.file_contents.insert(dst.to_path_buf(), c);
        }
        Ok(())
    }
    fn copy_file(&self, src: &Path, dst: &Path) -> Result<(), StepError> {
        let mut s = self.state.lock().expect("l");
        let content = s.file_contents.get(src).cloned().unwrap_or_default();
        s.paths.insert(dst.to_path_buf());
        s.file_contents.insert(dst.to_path_buf(), content);
        Ok(())
    }
    fn remove_file(&self, path: &Path) -> Result<(), StepError> {
        let mut s = self.state.lock().expect("l");
        s.paths.remove(path);
        s.file_contents.remove(path);
        s.modes.remove(path);
        Ok(())
    }
    fn append_line(&self, path: &Path, line: &str) -> Result<(), StepError> {
        let mut s = self.state.lock().expect("l");
        s.paths.insert(path.to_path_buf());
        let entry = s.file_contents.entry(path.to_path_buf()).or_default();
        entry.push_str(line);
        entry.push('\n');
        Ok(())
    }
    fn service_enable(&self, service: &str) -> Result<(), StepError> {
        self.state
            .lock()
            .expect("l")
            .svc_enabled
            .insert(service.to_string());
        Ok(())
    }
    fn service_disable(&self, service: &str) -> Result<(), StepError> {
        self.state.lock().expect("l").svc_enabled.remove(service);
        Ok(())
    }
    fn service_start(&self, service: &str) -> Result<(), StepError> {
        let mut s = self.state.lock().expect("l");
        s.svc_active.insert(service.to_string());
        if service == "nginx" {
            s.nginx_loaded_sites = Some(enabled_sites(&s));
        }
        Ok(())
    }
    fn service_stop(&self, service: &str) -> Result<(), StepError> {
        let mut s = self.state.lock().expect("l");
        s.svc_active.remove(service);
        if service == "nginx" {
            s.nginx_loaded_sites = None;
        }
        Ok(())
    }
    fn service_restart(&self, service: &str) -> Result<(), StepError> {
        let mut s = self.state.lock().expect("l");
        s.svc_active.insert(service.to_string());
        if service == "nginx" {
            s.nginx_loaded_sites = Some(enabled_sites(&s));
        }
        Ok(())
    }
    fn service_reload(&self, service: &str) -> Result<(), StepError> {
        let mut s = self.state.lock().expect("l");
        // a reload re-reads the disk, so the served config realigns with the
        // files present **at that moment** — only while the service runs.
        if service == "nginx" && s.svc_active.contains("nginx") {
            s.nginx_loaded_sites = Some(enabled_sites(&s));
        }
        Ok(())
    }
    fn daemon_reload(&self) -> Result<(), StepError> {
        Ok(())
    }
    fn pg_create_role(&self, role: &str, _pw: Option<&str>) -> Result<(), StepError> {
        self.state
            .lock()
            .expect("l")
            .pg_roles
            .insert(role.to_string());
        Ok(())
    }
    fn pg_drop_role(&self, role: &str) -> Result<(), StepError> {
        self.state.lock().expect("l").pg_roles.remove(role);
        Ok(())
    }
    fn createdb(&self, _owner: &str, db: &str) -> Result<(), StepError> {
        self.state.lock().expect("l").pg_dbs.insert(db.to_string());
        Ok(())
    }
    fn dropdb(&self, db: &str) -> Result<(), StepError> {
        let mut s = self.state.lock().expect("l");
        s.pg_dbs.remove(db);
        s.pg_initialized.remove(db);
        Ok(())
    }
    fn odoo_init_base(
        &self,
        _user: &str,
        _python: &Path,
        _odoo_bin: &Path,
        _conf: &Path,
        db: &str,
    ) -> Result<(), StepError> {
        self.state
            .lock()
            .expect("l")
            .pg_initialized
            .insert(db.to_string());
        Ok(())
    }
    fn git_clone(
        &self,
        _user: &str,
        _url: &str,
        _branch: &str,
        _depth: u32,
        target: &Path,
    ) -> Result<(), StepError> {
        let mut s = self.state.lock().expect("l");
        s.paths.insert(target.to_path_buf());
        s.paths.insert(target.join(".git"));
        s.paths.insert(target.join("odoo-bin"));
        let req = target.join("requirements.txt");
        s.paths.insert(req.clone());
        s.file_contents
            .insert(req, "gevent==21.12.0\npytz\n".to_string());
        Ok(())
    }
    fn tarball_install(&self, _user: &str, _url: &str, target: &Path) -> Result<(), StepError> {
        let mut s = self.state.lock().expect("l");
        s.paths.insert(target.to_path_buf());
        s.paths.insert(target.join("odoo-bin"));
        Ok(())
    }
    fn create_venv(&self, _user: &str, _python: &str, venv: &Path) -> Result<(), StepError> {
        let mut s = self.state.lock().expect("l");
        s.paths.insert(venv.to_path_buf());
        s.paths.insert(venv.join("bin").join("python3"));
        Ok(())
    }
    fn run_as_user(&self, _user: &str, _program: &str, _args: &[&str]) -> Result<(), StepError> {
        Ok(())
    }
}
