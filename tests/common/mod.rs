//! Mock di [`SystemOps`] condiviso dai test degli step privilegiati (Fase 3).
//!
//! Non esegue nulla: registra le operazioni richieste in un log condiviso e
//! risponde alle query da una configurazione statica. Così i test verificano la
//! logica di decisione (quale comando, con quali argomenti, in quale ramo
//! `PreState`) senza root e senza mutare il sistema.

#![allow(dead_code)] // non tutti i test usano tutte le utility

pub mod model;

use std::cell::Cell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use odoo_installer::error::StepError;
use odoo_installer::progress::ProgressReporter;
use odoo_installer::system_ops::{
    Downloader, OdooSourceState, OwnerId, PathKind, SystemOps, UserSpec,
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
    AptUpdate,
    AptInstall(Vec<String>),
    AptPurge(Vec<String>),
    AptAutoremove,
    AptFixBroken,
    DpkgConfigureAll,
    AptInstallDebFile(PathBuf),
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
    UfwDelete(String),
    ChownToUser {
        path: PathBuf,
        user: String,
    },
    AppendLine(PathBuf),
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

/// Risposte statiche del mock alle query di stato.
#[derive(Debug, Clone)]
pub struct MockConfig {
    pub user_exists: bool,
    pub path_exists: bool,
    pub owner: OwnerId,
    pub dir_empty: bool,
    /// Pacchetti che `dpkg_is_installed` considera già installati.
    pub installed_packages: HashSet<String>,
    /// Pacchetti per cui apt NON ha un candidato installabile: modella un nome
    /// che su questa release non esiste (`libtiff5-dev` su Debian 12, A5.1).
    /// Vuoto per default → ogni nome è installabile.
    pub packages_without_candidate: HashSet<String>,
    /// Pacchetti **virtuali**: nessun candidato reale (`apt-cache policy` dice
    /// `(none)`) ma `apt-get install` li sa risolvere via `Provides`. Modella
    /// `libfreetype6-dev` su Ubuntu 24.04 (A5.1-bis).
    pub virtual_packages: HashSet<String>,
    /// L'indice apt è popolato? `false` modella una macchina su cui
    /// `apt-get update` non è mai stato eseguito: **nessun** nome ha un
    /// candidato finché l'update non gira. È lo stato che ha prodotto il falso
    /// positivo A5.1-bis.
    pub apt_index_populated: bool,
    /// Versione riportata da `wkhtmltopdf_version` (None = non installato).
    pub wk_version: Option<String>,
    /// Stato iniziale del servizio (postgresql/odoo): enabled/active.
    pub service_enabled: bool,
    pub service_active: bool,
    /// Se `true`, start/restart NON portano il servizio ad attivo (simula un
    /// avvio fallito).
    pub service_start_fails: bool,
    /// Esistenza iniziale di ruolo/database PostgreSQL.
    pub role_exists: bool,
    pub db_exists: bool,
    /// Database non-template restituiti da `pg_list_databases` (cautela cluster).
    pub pg_databases_list: Vec<String>,
    /// Stato dei sorgenti Odoo rilevato da `detect_odoo_source`.
    pub source_state: OdooSourceState,
    /// Numero di tentativi di `git_clone` che falliscono prima di riuscire.
    pub git_clone_fail_times: u32,
    /// Se `true`, `tarball_install` fallisce.
    pub tarball_fails: bool,
    /// Se `true`, i fallimenti simulati delle operazioni di rete
    /// (`git_clone`, `tarball_install`) sono **timeout** (`StepError::Timeout`)
    /// invece di un exit code non-zero. Serve a verificare che un timeout sia
    /// trattato come un fallimento ritentabile come gli altri.
    pub network_failures_are_timeouts: bool,
    /// Il python del venv esiste già?
    pub venv_exists: bool,
    /// `python3 -m venv` disponibile?
    pub venv_available: bool,
    /// Contenuto di requirements.txt (None → read_to_string fallisce).
    pub requirements_content: Option<String>,
    /// Schema Odoo già presente nel DB?
    pub db_initialized: bool,
    /// Se `true`, le operazioni su file (write/move/copy/remove) toccano il
    /// filesystem reale (usare solo con path in una tempdir). chown resta finto.
    pub real_fs: bool,
    /// Nginx: il default site (`sites-enabled/default`) esiste?
    pub default_site_exists: bool,
    /// Nginx: **cosa** c'è al posto del default site (A-V3-5).
    ///
    /// `None` = coerente con `default_site_exists`: un symlink al target
    /// standard se esiste, altrimenti assente. Si valorizza esplicitamente per
    /// modellare i casi che il `bool` non sapeva distinguere — un file regolare,
    /// un symlink verso un target non standard.
    pub default_site_kind: Option<PathKind>,
    /// Nginx: il nostro symlink `sites-enabled/odoo<N>` esiste già?
    pub our_link_exists: bool,
    /// Firewall: ufw installato / attivo, e regole già presenti.
    pub ufw_available: bool,
    pub ufw_active: bool,
    pub existing_ufw_rules: HashSet<String>,
    /// `nginx -t` passa?
    pub nginx_test_ok: bool,
    /// Home restituita da `getent_home` (None → utente non trovato).
    pub sudo_home: Option<String>,
    /// `dpkg` parte in stato inconsistente: apt rifiuta di operare finché un
    /// `apt-get install -f` o un `dpkg --configure -a` non lo sistema. È lo
    /// stato osservato sulla VM di prova dopo un `dpkg -i` con deps mancanti.
    pub dpkg_broken: bool,
    /// `apt-get install -f` non riesce a riparare (recovery davvero fallito).
    pub fix_broken_fails: bool,
    /// `dpkg --configure -a` non riesce a riparare.
    pub dpkg_configure_fails: bool,
    /// `apt-get install -y <deb>` fallisce (errore vero, non deps mancanti).
    pub apt_install_deb_fails: bool,
    /// `apt-get update` esce non-zero. Da solo modella il repository di terze
    /// parti irraggiungibile (l'indice resta popolato); insieme a
    /// `apt_index_populated: false` modella il fallimento vero, senza rete.
    pub apt_update_fails: bool,
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
    // Schema DB: flippa a true dopo odoo_init_base.
    db_initialized: Cell<bool>,
    // Stato di dpkg: parte da cfg.dpkg_broken, lo rimettono a posto
    // apt_fix_broken / dpkg_configure_all / apt_install_deb_file.
    dpkg_broken: Cell<bool>,
    // Indice apt: parte da cfg.apt_index_populated e lo accende `apt_update`.
    // È ciò che rende il mock capace di riprodurre la sequenza reale
    // "bootstrap aggiorna → i deps trovano i candidati" (A5.1-bis).
    index_populated: Cell<bool>,
}

/// L'errore che apt restituisce quando `dpkg` è in stato inconsistente —
/// verbatim da quello osservato sulla VM di prova.
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
    /// Crea il mock e ritorna l'handle al log per le asserzioni.
    pub fn new(cfg: MockConfig) -> (Self, OpLog) {
        let log: OpLog = Arc::new(Mutex::new(Vec::new()));
        (Self::with_log(cfg, Arc::clone(&log)), log)
    }

    /// Crea il mock su un log condiviso (per verificare l'ordine tra più step).
    pub fn with_log(cfg: MockConfig, log: OpLog) -> Self {
        let active = Cell::new(cfg.service_active);
        let enabled = Cell::new(cfg.service_enabled);
        let db_initialized = Cell::new(cfg.db_initialized);
        let dpkg_broken = Cell::new(cfg.dpkg_broken);
        let index_populated = Cell::new(cfg.apt_index_populated);
        MockSystemOps {
            log,
            cfg,
            active,
            enabled,
            git_clone_calls: Cell::new(0),
            db_initialized,
            dpkg_broken,
            index_populated,
        }
    }

    fn record(&self, op: Op) {
        if let Ok(mut entries) = self.log.lock() {
            entries.push(op);
        }
    }

    /// Il fallimento simulato di un'operazione di rete: timeout o exit code
    /// non-zero, secondo `network_failures_are_timeouts`.
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

impl SystemOps for MockSystemOps {
    fn user_exists(&self, _user: &str) -> bool {
        self.cfg.user_exists
    }
    fn path_exists(&self, path: &Path) -> bool {
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

    fn dpkg_is_installed(&self, pkg: &str) -> bool {
        self.cfg.installed_packages.contains(pkg)
    }
    fn apt_update(&self) -> Result<(), StepError> {
        self.record(Op::AptUpdate);
        if self.cfg.apt_update_fails {
            return Err(StepError::CommandFailed {
                command: "apt-get update".to_string(),
                status: "100".to_string(),
                stderr: "E: Some index files failed to download (simulato)".to_string(),
            });
        }
        // L'update popola l'indice: da qui in poi le interrogazioni rispondono.
        self.index_populated.set(true);
        Ok(())
    }
    fn apt_has_real_candidate(&self, pkg: &str) -> bool {
        // Senza indice nessuna interrogazione risponde: è il caso che in campo
        // ha prodotto il falso positivo su un pacchetto standard.
        if !self.index_populated.get() {
            return false;
        }
        !self.cfg.packages_without_candidate.contains(pkg)
            && !self.cfg.virtual_packages.contains(pkg)
    }
    fn apt_can_install(&self, pkg: &str) -> bool {
        if !self.index_populated.get() {
            return false;
        }
        // I virtuali sono installabili anche senza candidato reale.
        self.cfg.virtual_packages.contains(pkg)
            || !self.cfg.packages_without_candidate.contains(pkg)
    }
    fn apt_index_is_populated(&self) -> bool {
        self.index_populated.get()
    }
    fn apt_install(&self, pkgs: &[&str]) -> Result<(), StepError> {
        self.record(Op::AptInstall(pkgs.iter().map(|s| s.to_string()).collect()));
        Ok(())
    }
    fn apt_purge(&self, pkgs: &[&str]) -> Result<(), StepError> {
        self.record(Op::AptPurge(pkgs.iter().map(|s| s.to_string()).collect()));
        // Modella A-RT-2: con dpkg rotto apt si rifiuta di operare, finché un
        // fix-broken (o un `dpkg --configure -a`) non lo rimette a posto.
        if self.dpkg_broken.get() {
            return Err(unmet_dependencies("apt-get purge"));
        }
        Ok(())
    }
    fn apt_autoremove(&self) -> Result<(), StepError> {
        self.record(Op::AptAutoremove);
        Ok(())
    }
    fn apt_fix_broken(&self) -> Result<(), StepError> {
        self.record(Op::AptFixBroken);
        if self.cfg.fix_broken_fails {
            return Err(unmet_dependencies("apt-get install -f"));
        }
        self.dpkg_broken.set(false);
        Ok(())
    }
    fn dpkg_configure_all(&self) -> Result<(), StepError> {
        self.record(Op::DpkgConfigureAll);
        if self.cfg.dpkg_configure_fails {
            return Err(unmet_dependencies("dpkg --configure -a"));
        }
        self.dpkg_broken.set(false);
        Ok(())
    }
    fn apt_install_deb_file(&self, path: &Path) -> Result<(), StepError> {
        self.record(Op::AptInstallDebFile(path.to_path_buf()));
        if self.cfg.apt_install_deb_fails {
            return Err(StepError::CommandFailed {
                command: "apt-get install -y -- <deb>".to_string(),
                status: "100".to_string(),
                stderr: "impossibile installare il .deb (simulato)".to_string(),
            });
        }
        // apt risolve le dipendenze: dpkg resta consistente.
        self.dpkg_broken.set(false);
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
    fn ufw_available(&self) -> bool {
        self.cfg.ufw_available
    }
    fn ufw_is_active(&self) -> bool {
        self.cfg.ufw_active
    }
    /// Risponde **come il vero `ufw`**: rende un output in stile `ufw status` a
    /// partire dalle regole configurate e lo interroga con la stessa funzione
    /// che usa la produzione.
    ///
    /// Prima era `existing_ufw_rules.contains(rule)`, cioè un'appartenenza a un
    /// insieme: una semantica ideale che il comando reale non ha. È il motivo
    /// per cui nessun test poteva accorgersi di A-V3-7 — il confronto per
    /// sottostringa sbagliava su `8080/tcp`, ma il mock non lo riproduceva.
    fn ufw_rule_exists(&self, rule: &str) -> Result<bool, StepError> {
        let mut status = String::from("Status: active\n\nTo   Action   From\n--   ------   ----\n");
        for existing in &self.cfg.existing_ufw_rules {
            status.push_str(&format!(
                "{existing}                   ALLOW       Anywhere\n"
            ));
        }
        Ok(odoo_installer::system_ops::ufw_rule_in_status(
            &status, rule,
        ))
    }
    fn ufw_allow(&self, rule: &str) -> Result<(), StepError> {
        self.record(Op::UfwAllow(rule.to_string()));
        Ok(())
    }
    fn ufw_delete(&self, rule: &str) -> Result<(), StepError> {
        self.record(Op::UfwDelete(rule.to_string()));
        Ok(())
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
    fn pg_list_databases(&self) -> Result<Vec<String>, StepError> {
        Ok(self.cfg.pg_databases_list.clone())
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
    fn python_venv_available(&self) -> bool {
        self.cfg.venv_available
    }
    fn create_venv(&self, _user: &str, venv: &Path) -> Result<(), StepError> {
        self.record(Op::CreateVenv(venv.to_path_buf()));
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
            // Stesse garanzie del reale: O_EXCL | O_NOFOLLOW.
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

/// Reporter di progresso che registra gli eventi come stringhe (per i test).
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
