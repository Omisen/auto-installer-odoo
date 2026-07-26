//! Mock di [`SystemOps`] condiviso dai test degli step privilegiati (Fase 3).
//!
//! Non esegue nulla: registra le operazioni richieste in un log condiviso e
//! risponde alle query da una configurazione statica. Così i test verificano la
//! logica di decisione (quale comando, con quali argomenti, in quale ramo
//! `PreState`) senza root e senza mutare il sistema.

#![allow(dead_code)] // non tutti i test usano tutte le utility

use std::cell::Cell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use odoo_installer::error::StepError;
use odoo_installer::system_ops::{
    Downloader, OdooSourceState, OwnerId, SystemOps, UserSpec,
};

/// Operazione mutante registrata dal mock.
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
    AptInstall(Vec<String>),
    AptPurge(Vec<String>),
    AptAutoremove,
    AptFixBroken,
    DpkgInstallFile(PathBuf),
    Download {
        url: String,
        dest: PathBuf,
    },
    ServiceEnable(String),
    ServiceDisable(String),
    ServiceStart(String),
    ServiceStop(String),
    PgCreateRole {
        role: String,
        // Solo se la password è presente — MAI il valore, per non registrarlo.
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
    CreateVenv(PathBuf),
}

/// Risposte statiche del mock alle query di stato.
#[derive(Debug, Clone)]
pub struct MockConfig {
    pub user_exists: bool,
    pub path_exists: bool,
    pub owner: OwnerId,
    pub dir_empty: bool,
    /// Pacchetti che `dpkg_is_installed` considera già installati.
    pub installed_packages: HashSet<String>,
    /// Versione riportata da `wkhtmltopdf_version` (None = non installato).
    pub wk_version: Option<String>,
    /// Stato iniziale del servizio (postgresql): enabled/active.
    pub service_enabled: bool,
    pub service_active: bool,
    /// Esistenza iniziale di ruolo/database PostgreSQL.
    pub role_exists: bool,
    pub db_exists: bool,
    /// Stato dei sorgenti Odoo rilevato da `detect_odoo_source`.
    pub source_state: OdooSourceState,
    /// Numero di tentativi di `git_clone` che falliscono prima di riuscire.
    pub git_clone_fail_times: u32,
    /// Se `true`, `tarball_install` fallisce.
    pub tarball_fails: bool,
    /// Il python del venv esiste già?
    pub venv_exists: bool,
    /// `python3 -m venv` disponibile?
    pub venv_available: bool,
    /// Contenuto di requirements.txt (None → read_to_string fallisce).
    pub requirements_content: Option<String>,
}

impl Default for MockConfig {
    fn default() -> Self {
        MockConfig {
            user_exists: false,
            path_exists: false,
            owner: OwnerId { uid: 0, gid: 0 },
            dir_empty: true,
            installed_packages: HashSet::new(),
            wk_version: None,
            service_enabled: false,
            service_active: false,
            role_exists: false,
            db_exists: false,
            source_state: OdooSourceState::Absent,
            git_clone_fail_times: 0,
            tarball_fails: false,
            venv_exists: false,
            venv_available: true,
            requirements_content: None,
        }
    }
}

/// Handle condiviso al log delle operazioni, ispezionabile dai test dopo che lo
/// step ha preso possesso del mock.
pub type OpLog = Arc<Mutex<Vec<Op>>>;

pub struct MockSystemOps {
    log: OpLog,
    cfg: MockConfig,
    // Stato del servizio con interior mutability: start/stop/enable/disable lo
    // aggiornano, così la verifica post-start di SetupPostgres funziona.
    active: Cell<bool>,
    enabled: Cell<bool>,
    // Conteggio chiamate a git_clone (per simulare i fallimenti iniziali).
    git_clone_calls: Cell<u32>,
}

impl MockSystemOps {
    /// Crea il mock e ritorna l'handle al log per le asserzioni.
    pub fn new(cfg: MockConfig) -> (Self, OpLog) {
        let log: OpLog = Arc::new(Mutex::new(Vec::new()));
        (Self::with_log(cfg, Arc::clone(&log)), log)
    }

    /// Crea il mock su un log condiviso (per verificare l'ordine tra più step).
    pub fn with_log(cfg: MockConfig, log: OpLog) -> Self {
        let active = Cell::new(cfg.service_active);
        let enabled = Cell::new(cfg.service_enabled);
        MockSystemOps {
            log,
            cfg,
            active,
            enabled,
            git_clone_calls: Cell::new(0),
        }
    }

    fn record(&self, op: Op) {
        if let Ok(mut entries) = self.log.lock() {
            entries.push(op);
        }
    }
}

impl SystemOps for MockSystemOps {
    fn user_exists(&self, _user: &str) -> bool {
        self.cfg.user_exists
    }
    fn path_exists(&self, _path: &Path) -> bool {
        self.cfg.path_exists
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

    fn dpkg_is_installed(&self, pkg: &str) -> bool {
        self.cfg.installed_packages.contains(pkg)
    }
    fn apt_install(&self, pkgs: &[&str]) -> Result<(), StepError> {
        self.record(Op::AptInstall(pkgs.iter().map(|s| s.to_string()).collect()));
        Ok(())
    }
    fn apt_purge(&self, pkgs: &[&str]) -> Result<(), StepError> {
        self.record(Op::AptPurge(pkgs.iter().map(|s| s.to_string()).collect()));
        Ok(())
    }
    fn apt_autoremove(&self) -> Result<(), StepError> {
        self.record(Op::AptAutoremove);
        Ok(())
    }
    fn apt_fix_broken(&self) -> Result<(), StepError> {
        self.record(Op::AptFixBroken);
        Ok(())
    }
    fn dpkg_install_file(&self, path: &Path) -> Result<(), StepError> {
        self.record(Op::DpkgInstallFile(path.to_path_buf()));
        Ok(())
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
        self.active.set(true);
        self.record(Op::ServiceStart(service.to_string()));
        Ok(())
    }
    fn service_stop(&self, service: &str) -> Result<(), StepError> {
        self.active.set(false);
        self.record(Op::ServiceStop(service.to_string()));
        Ok(())
    }

    fn pg_role_exists(&self, _role: &str) -> Result<bool, StepError> {
        Ok(self.cfg.role_exists)
    }
    fn pg_db_exists(&self, _db: &str) -> Result<bool, StepError> {
        Ok(self.cfg.db_exists)
    }
    fn pg_create_role(&self, role: &str, password: Option<&str>) -> Result<(), StepError> {
        // Registra SOLO la presenza della password, mai il valore.
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

    fn run_as_user(&self, user: &str, program: &str, args: &[&str]) -> Result<(), StepError> {
        self.record(Op::RunAsUser {
            user: user.to_string(),
            program: program.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
        });
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
            Err(StepError::CommandFailed {
                command: "git clone".to_string(),
                status: "1".to_string(),
                stderr: "fallimento clone simulato".to_string(),
            })
        } else {
            Ok(())
        }
    }
    fn tarball_install(&self, _user: &str, _url: &str, target: &Path) -> Result<(), StepError> {
        self.record(Op::TarballInstall {
            target: target.to_path_buf(),
        });
        if self.cfg.tarball_fails {
            Err(StepError::Precondition("tarball fallito (simulato)".to_string()))
        } else {
            Ok(())
        }
    }
    fn venv_python_exists(&self, _venv: &Path) -> bool {
        self.cfg.venv_exists
    }
    fn python_venv_available(&self) -> bool {
        self.cfg.venv_available
    }
    fn create_venv(&self, _user: &str, venv: &Path) -> Result<(), StepError> {
        self.record(Op::CreateVenv(venv.to_path_buf()));
        Ok(())
    }
    fn read_to_string(&self, path: &Path) -> Result<String, StepError> {
        self.cfg.requirements_content.clone().ok_or_else(|| {
            StepError::io(path, std::io::Error::from(std::io::ErrorKind::NotFound))
        })
    }
}

/// Downloader mock: scrive `bytes` in `dest` (per far calcolare uno SHA-256
/// reale nei test) e registra il download nel log condiviso.
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

/// Snapshot del log per le asserzioni.
pub fn ops_of(log: &OpLog) -> Vec<Op> {
    log.lock().expect("lock").clone()
}
