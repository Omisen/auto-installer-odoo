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

use odoo_installer::error::StepError;
use odoo_installer::system_ops::{OdooSourceState, OwnerId, SystemOps, UserSpec};

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
    pub file_contents: HashMap<PathBuf, String>,
    // Ambiente (non muta): non entra nel confronto oltre a ciò che sopra copre.
    pub ufw_available: bool,
    pub ufw_active: bool,
    pub wk_version: Option<String>,
    pub sudo_home: Option<String>,
}

/// Handle condiviso al modello.
#[derive(Clone)]
pub struct SystemModel {
    state: Arc<Mutex<ModelState>>,
}

impl SystemModel {
    pub fn new(state: ModelState) -> Self {
        SystemModel {
            state: Arc::new(Mutex::new(state)),
        }
    }
    /// Un altro handle allo stesso stato (per un altro step).
    pub fn handle(&self) -> SystemModel {
        SystemModel {
            state: Arc::clone(&self.state),
        }
    }
    /// Snapshot dello stato corrente (per il confronto inizio/fine).
    pub fn snapshot(&self) -> ModelState {
        self.state.lock().expect("lock").clone()
    }
    pub fn boxed(&self) -> Box<dyn SystemOps> {
        Box::new(self.handle())
    }
}

fn under(entry: &Path, dir: &Path) -> bool {
    entry != dir && entry.starts_with(dir)
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
    fn dpkg_is_installed(&self, pkg: &str) -> bool {
        self.state.lock().expect("l").packages.contains(pkg)
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
    fn ufw_available(&self) -> bool {
        self.state.lock().expect("l").ufw_available
    }
    fn ufw_is_active(&self) -> bool {
        self.state.lock().expect("l").ufw_active
    }
    fn ufw_rule_exists(&self, rule: &str) -> Result<bool, StepError> {
        Ok(self.state.lock().expect("l").ufw_rules.contains(rule))
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
    fn apt_install(&self, pkgs: &[&str]) -> Result<(), StepError> {
        let mut s = self.state.lock().expect("l");
        for p in pkgs {
            s.packages.insert(p.to_string());
        }
        Ok(())
    }
    fn apt_purge(&self, pkgs: &[&str]) -> Result<(), StepError> {
        let mut s = self.state.lock().expect("l");
        for p in pkgs {
            s.packages.remove(*p);
            if *p == "wkhtmltox" {
                s.wk_version = None;
            }
        }
        Ok(())
    }
    fn apt_autoremove(&self) -> Result<(), StepError> {
        Ok(())
    }
    fn apt_fix_broken(&self) -> Result<(), StepError> {
        Ok(())
    }
    fn dpkg_install_file(&self, _path: &Path) -> Result<(), StepError> {
        let mut s = self.state.lock().expect("l");
        s.packages.insert("wkhtmltox".to_string());
        s.wk_version = Some("0.12.6.1".to_string());
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
        self.state
            .lock()
            .expect("l")
            .svc_active
            .insert(service.to_string());
        Ok(())
    }
    fn service_stop(&self, service: &str) -> Result<(), StepError> {
        self.state.lock().expect("l").svc_active.remove(service);
        Ok(())
    }
    fn service_restart(&self, service: &str) -> Result<(), StepError> {
        self.state
            .lock()
            .expect("l")
            .svc_active
            .insert(service.to_string());
        Ok(())
    }
    fn service_reload(&self, _service: &str) -> Result<(), StepError> {
        Ok(())
    }
    fn daemon_reload(&self) -> Result<(), StepError> {
        Ok(())
    }
    fn ufw_allow(&self, rule: &str) -> Result<(), StepError> {
        self.state
            .lock()
            .expect("l")
            .ufw_rules
            .insert(rule.to_string());
        Ok(())
    }
    fn ufw_delete(&self, rule: &str) -> Result<(), StepError> {
        self.state.lock().expect("l").ufw_rules.remove(rule);
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
