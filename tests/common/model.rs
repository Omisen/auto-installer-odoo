//! `SystemModel`: modello **stateful e coerente** del sistema, per i test di
//! rollback end-to-end (Fase 12).
//!
//! A differenza del mock che registra le operazioni, qui ogni operazione mutante
//! aggiorna uno stato condiviso e ogni undo lo ripristina. Così "il sistema è
//! tornato al vergine" è verificabile: `stato_finale == stato_iniziale`.
//!
//! Tutti gli step di una sequenza condividono lo **stesso** modello (handle
//! `Arc`), così le mutazioni di uno step sono viste dagli undo di un altro.

#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use odoo_installer::distro::{Distro, Firewall};
use odoo_installer::error::StepError;
use odoo_installer::packaging::{Availability, PackageCatalog, PackageManager};
use odoo_installer::system_ops::{OdooSourceState, OwnerId, PathKind, SystemOps, UserSpec};

/// Stato del sistema modellato. `PartialEq` per confrontare inizio/fine.
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
    /// Config Nginx **caricata in memoria** dal processo in esecuzione: i siti
    /// abilitati al momento dell'ultimo start/reload. `None` = nginx non attivo.
    ///
    /// Modella la differenza fra *config su disco* e *config servita*: un
    /// `nginx` che gira continua a servire ciò che ha caricato, anche se i file
    /// sono cambiati sotto di lui. Senza questa distinzione un rollback può
    /// sembrare completo (i file sono a posto) mentre il servizio del cliente
    /// sta ancora servendo la nostra config.
    pub nginx_loaded_sites: Option<HashSet<PathBuf>>,
    /// `dpkg` in stato inconsistente: apt rifiuta di operare finché un
    /// `apt-get install -f` o un `dpkg --configure -a` non lo sistema.
    pub dpkg_broken: bool,
    /// Dipendenze di sistema che un `.deb` locale richiede e che non sono
    /// presenti (es. `fontconfig`, `xfonts-base` su una VM minimale): le
    /// installa chi risolve le dipendenze, cioè apt.
    pub pending_deps: HashSet<String>,
    pub file_contents: HashMap<PathBuf, String>,
    // Ambiente (non muta): non entra nel confronto oltre a ciò che sopra copre.
    pub ufw_available: bool,
    pub ufw_active: bool,
    pub wk_version: Option<String>,
    pub sudo_home: Option<String>,
    /// Nomi di pacchetto che su questa "release" non esistono: apt non ha un
    /// candidato installabile (A5.1). Vuoto = tutto installabile.
    pub packages_without_candidate: HashSet<String>,
    /// Nomi **virtuali**: nessun candidato reale, ma apt li risolve via
    /// `Provides` (A5.1-bis).
    pub virtual_packages: HashSet<String>,
    /// L'indice apt parte **stantìo** (macchina su cui `apt-get update` non è
    /// mai stato eseguito). Default `false` = macchina normale.
    ///
    /// È solo la *condizione iniziale*: lo stato dell'indice vive fuori da
    /// `ModelState` (vedi [`SystemModel`]) proprio perché `apt_update` lo cambia
    /// e il confronto inizio/fine non deve accorgersene — un indice aggiornato
    /// non è un artefatto da annullare, è una cache.
    pub apt_index_stale: bool,
}

/// Handle condiviso al modello.
#[derive(Clone)]
pub struct SystemModel {
    state: Arc<Mutex<ModelState>>,
    /// Indice apt: condiviso fra gli handle (l'update di uno step si vede dagli
    /// altri) ma **fuori** da `ModelState`, quindi invisibile al confronto
    /// inizio/fine. Aggiornare l'indice non è un artefatto da annullare.
    apt_index_populated: Arc<Mutex<bool>>,
    packages: ModelPackages,
    distro: ModelDistro,
}

impl SystemModel {
    pub fn new(state: ModelState) -> Self {
        let apt_index_populated = Arc::new(Mutex::new(!state.apt_index_stale));
        Self::from_parts(Arc::new(Mutex::new(state)), apt_index_populated)
    }
    /// Un altro handle allo stesso stato (per un altro step).
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
    /// Snapshot dello stato corrente (per il confronto inizio/fine).
    pub fn snapshot(&self) -> ModelState {
        self.state.lock().expect("lock").clone()
    }
    /// Modifica lo stato dall'esterno: serve agli step di test che devono
    /// simulare un danno collaterale (es. "questo step ha lasciato dpkg rotto").
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

/// L'errore che apt restituisce quando `dpkg` è in stato inconsistente.
fn unmet_dependencies() -> StepError {
    StepError::CommandFailed {
        command: "apt-get".to_string(),
        status: "100".to_string(),
        stderr: "E: Unmet dependencies. Try 'apt --fix-broken install' with no packages (or \
                 specify a solution)."
            .to_string(),
    }
}

/// Directory dei siti Nginx abilitati (symlink).
pub const SITES_ENABLED: &str = "/etc/nginx/sites-enabled";

/// I siti attualmente abilitati **su disco**.
fn enabled_sites(state: &ModelState) -> HashSet<PathBuf> {
    let dir = Path::new(SITES_ENABLED);
    state
        .symlinks
        .iter()
        .filter(|l| under(l, dir))
        .cloned()
        .collect()
}

/// Il gestore di pacchetti del modello: condivide lo stesso stato del
/// [`SystemModel`] che lo possiede, così `apt-get install` di uno step si vede
/// dall'undo di un altro — che è tutto il punto di questo modello.
#[derive(Clone)]
pub struct ModelPackages {
    state: Arc<Mutex<ModelState>>,
    apt_index_populated: Arc<Mutex<bool>>,
}

impl PackageManager for ModelPackages {
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
        // A-RT-2: su un dpkg rotto apt si rifiuta di operare. È ciò che sulla VM
        // di prova lasciava installati i 24 pacchetti del delta.
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
        // Installa le dipendenze mancanti e configura ciò che era a metà.
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
        // apt risolve le dipendenze di sistema del .deb in un colpo solo: il
        // pacchetto è configurato e dpkg resta consistente.
        s.packages.insert("wkhtmltox".to_string());
        let deps = std::mem::take(&mut s.pending_deps);
        s.packages.extend(deps);
        s.wk_version = Some("0.12.6.1".to_string());
        Ok(())
    }
    fn local_package_name(&self, version: &str, suffix: &str) -> String {
        odoo_installer::packaging::apt::AptBackend.local_package_name(version, suffix)
    }

    fn refresh_command(&self) -> &'static str {
        odoo_installer::packaging::apt::AptBackend.refresh_command()
    }

    fn catalog(&self) -> PackageCatalog {
        odoo_installer::packaging::apt::AptBackend.catalog()
    }
}

/// Il firewall del modello.
#[derive(Clone)]
pub struct ModelFirewall {
    state: Arc<Mutex<ModelState>>,
}

impl Firewall for ModelFirewall {
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

/// Le convenzioni di distribuzione del modello.
#[derive(Clone)]
pub struct ModelDistro {
    firewall: ModelFirewall,
}

impl Distro for ModelDistro {
    fn firewall(&self) -> &dyn Firewall {
        &self.firewall
    }

    /// Il modello rappresenta una macchina Debian: il cluster lo crea il
    /// pacchetto, non noi.
    fn postgres_data_dir(&self) -> Option<PathBuf> {
        None
    }

    fn init_postgres_cluster(&self) -> Result<(), StepError> {
        Ok(())
    }
}

impl SystemOps for SystemModel {
    // --- query ---------------------------------------------------------------
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
            // Il modello non traccia il target dei symlink: per il default site
            // vale quello standard, che è ciò che una macchina reale ha.
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
    fn python_venv_available(&self) -> bool {
        true
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

    // --- mutazioni (con undo simmetrico) -------------------------------------
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
    fn chmod(&self, _p: &Path, _m: u32) -> Result<(), StepError> {
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
        self.state.lock().expect("l").paths.remove(path);
        Ok(())
    }
    fn remove_dir_all(&self, path: &Path) -> Result<(), StepError> {
        let mut s = self.state.lock().expect("l");
        s.paths.retain(|e| e != path && !e.starts_with(path));
        s.symlinks.retain(|e| e != path && !e.starts_with(path));
        s.file_contents
            .retain(|k, _| k != path && !k.starts_with(path));
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
        self.state.lock().expect("l").symlinks.remove(link);
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
        // Modella `O_EXCL`: creare sopra un path esistente è un errore.
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
        // Un reload rilegge il disco: la config servita si riallinea ai file
        // **presenti in quel momento**. Solo se il servizio è attivo.
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
    fn create_venv(&self, _user: &str, venv: &Path) -> Result<(), StepError> {
        let mut s = self.state.lock().expect("l");
        s.paths.insert(venv.to_path_buf());
        s.paths.insert(venv.join("bin").join("python3"));
        Ok(())
    }
    fn run_as_user(&self, _user: &str, _program: &str, _args: &[&str]) -> Result<(), StepError> {
        Ok(())
    }
}
