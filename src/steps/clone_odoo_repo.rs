//! [`CloneOdooRepo`] (9a): clona i sorgenti Odoo in `<install_dir>/odoo`.
//!
//! È il primo dei tre sotto-step in cui spezziamo il monolitico `install_odoo`
//! del Bash. Tutte le operazioni girano come **utente odoo** (privilegio
//! minimo), non root.
//!
//! # Perimetro e coordinamento del contenitore
//!
//! Questo step crea `<install_dir>` (es. `/opt/odoo/odoo18`) e le sue
//! sottocartelle. **Non** tocca `/opt/odoo`, che è di
//! [`PrepareOptRoot`](crate::steps::prepare_opt_root): `<install_dir>` è un
//! livello sotto, interamente nostro. Qui `rm -rf` è legittimo (è la nostra
//! cartella di sorgenti). L'undo rimuove il repo e, come per `/opt/odoo` in
//! Fase 2, rimuove il **contenitore** `<install_dir>` solo se vuoto (dopo che
//! gli undo di venv/config sono girati prima, in ordine inverso).

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};
use crate::system_ops::{OdooSourceState, SystemOps};

const ODOO_GIT_URL: &str = "https://github.com/odoo/odoo.git";
const REPO_SUBDIR: &str = "odoo";
const REPOS_SUBDIR: &str = "repos";
const MODULES_SUBDIR: &str = "repos/modules";
const DEFAULT_DEPTH: u32 = 5;
const DEFAULT_RETRIES: u32 = 3;
const DEFAULT_BACKOFF_SECS: u64 = 2;

/// Snapshot serializzabile del clone.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CloneSnapshot {
    pub prestate: PreState,
    /// git | git-existing | tarball | tarball-existing | "" (assente).
    pub source_mode: String,
}

/// Clona i sorgenti Odoo (reversibile) con retry + fallback tarball.
pub struct CloneOdooRepo {
    ops: Box<dyn SystemOps>,
    retries: u32,
    depth: u32,
    backoff_base_secs: u64,
    snap: CloneSnapshot,
    /// Dir esistente ma non valida da rigenerare (rimossa prima del clone).
    had_invalid_dir: bool,
}

impl CloneOdooRepo {
    /// Costruttore di **produzione**: legge i parametri di rete dall'ambiente e
    /// applica il backoff reale fra un tentativo di clone e il successivo.
    ///
    /// Non è la stessa cosa di [`Self::with_ops`], che azzera il backoff perché i
    /// test non devono dormire. Costruire questo step per il `run` con il
    /// costruttore dei test renderebbe il retry di R2 **inutile**: tre tentativi
    /// istantanei su una rete che non risponde sono un tentativo solo.
    pub fn for_run(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            retries: env_u32("GIT_CLONE_RETRIES", DEFAULT_RETRIES),
            depth: env_u32("GIT_DEPTH", DEFAULT_DEPTH),
            backoff_base_secs: DEFAULT_BACKOFF_SECS,
            snap: CloneSnapshot::default(),
            had_invalid_dir: false,
        }
    }

    /// Costruttore per i test: `SystemOps` iniettabile, **nessun backoff** (sleep 0).
    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            retries: DEFAULT_RETRIES,
            depth: DEFAULT_DEPTH,
            backoff_base_secs: 0,
            snap: CloneSnapshot::default(),
            had_invalid_dir: false,
        }
    }

    fn repo_dir(ctx: &Context) -> std::path::PathBuf {
        ctx.install_dir.join(REPO_SUBDIR)
    }

    fn sleep_backoff(&self, failed_attempt: u32) {
        if self.backoff_base_secs > 0 {
            std::thread::sleep(std::time::Duration::from_secs(
                failed_attempt as u64 * self.backoff_base_secs,
            ));
        }
    }
}

/// Legge una variabile d'ambiente come `u32`, con default.
fn env_u32(key: &str, default: u32) -> u32 {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

impl Step for CloneOdooRepo {
    fn name(&self) -> &str {
        "clone-odoo-repo"
    }

    fn snapshot(&mut self, ctx: &Context) -> Result<(), StepError> {
        let repo_dir = Self::repo_dir(ctx);
        match self.ops.detect_odoo_source(&ctx.odoo_user, &repo_dir)? {
            OdooSourceState::Absent => {
                self.snap.prestate = PreState::Untracked;
                self.snap.source_mode.clear();
            }
            OdooSourceState::GitRepo { branch } => {
                if branch == ctx.odoo_version {
                    self.snap.prestate = PreState::Preexisting;
                    self.snap.source_mode = "git-existing".to_string();
                } else {
                    // Branch diverso: potrebbe contenere lavoro. NON rigenerare.
                    return Err(StepError::Precondition(format!(
                        "repository esistente su branch '{branch}', atteso '{}'. \
                         Rimuovi manualmente {} se vuoi ri-clonare",
                        ctx.odoo_version,
                        repo_dir.display()
                    )));
                }
            }
            OdooSourceState::TarballPresent => {
                self.snap.prestate = PreState::Preexisting;
                self.snap.source_mode = "tarball-existing".to_string();
            }
            OdooSourceState::InvalidDir => {
                // Dir sotto <install_dir> (nostro perimetro) senza .git né
                // odoo-bin: non è un checkout valido. La rigeneriamo, ma lo
                // segnaliamo esplicitamente.
                warn!(
                    dir = %repo_dir.display(),
                    "directory sorgenti esistente ma non valida: verrà rigenerata"
                );
                self.snap.prestate = PreState::Untracked;
                self.had_invalid_dir = true;
            }
        }
        info!(prestate = ?self.snap.prestate, mode = %self.snap.source_mode, "snapshot clone-odoo-repo");
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        if self.snap.prestate == PreState::Preexisting {
            info!(mode = %self.snap.source_mode, "run: sorgenti già presenti, skip clone");
            return Ok(());
        }
        if ctx.dry_run {
            info!("run (dry-run): creerei le directory e clonerei i sorgenti Odoo");
            return Ok(());
        }

        let user = &ctx.odoo_user;
        let repo_dir = Self::repo_dir(ctx);

        // Struttura directory come utente odoo.
        self.ops.mkdir_p_as_user(user, &ctx.install_dir)?;
        self.ops
            .mkdir_p_as_user(user, &ctx.install_dir.join(MODULES_SUBDIR))?;

        // Directory non valida preesistente → rimuovila prima di clonare.
        if self.had_invalid_dir {
            self.ops.remove_dir_all(&repo_dir)?;
        }

        // Clone con retry + backoff.
        let mut cloned = false;
        for attempt in 1..=self.retries {
            match self
                .ops
                .git_clone(user, ODOO_GIT_URL, &ctx.odoo_version, self.depth, &repo_dir)
            {
                Ok(()) => {
                    self.snap.source_mode = "git".to_string();
                    cloned = true;
                    info!(attempt, "run: clone completato");
                    break;
                }
                Err(e) => {
                    warn!(attempt, retries = self.retries, error = %e, "run: clone fallito");
                    // Pulisci artefatti parziali prima del retry.
                    let _ = self.ops.remove_dir_all(&repo_dir);
                    if attempt < self.retries {
                        self.sleep_backoff(attempt);
                    }
                }
            }
        }

        // Fallback tarball se il clone è fallito dopo tutti i tentativi.
        if !cloned {
            warn!(
                retries = self.retries,
                "run: clone fallito, attivo fallback tarball"
            );
            let tar_url = format!(
                "https://codeload.github.com/odoo/odoo/tar.gz/refs/heads/{}",
                ctx.odoo_version
            );
            self.ops.tarball_install(user, &tar_url, &repo_dir)?;
            self.snap.source_mode = "tarball".to_string();
            info!("run: sorgenti installate via tarball fallback");
        }

        self.snap.prestate = PreState::CreatedByUs;
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        if self.snap.prestate != PreState::CreatedByUs {
            info!(prestate = ?self.snap.prestate, "undo NO-OP (sorgenti non create da noi)");
            return Ok(());
        }
        if ctx.dry_run {
            info!("undo (dry-run): rm -rf dei sorgenti + rimozione contenitore se vuoto");
            return Ok(());
        }

        let repo_dir = Self::repo_dir(ctx);
        // rm -rf del target: nostro perimetro, legittimo.
        if let Err(e) = self.ops.remove_dir_all(&repo_dir) {
            warn!(error = %e, "undo: rm -rf sorgenti fallito, proseguo (best-effort)");
        }
        // repos/modules creata da noi.
        if let Err(e) = self.ops.remove_dir_all(&ctx.install_dir.join(REPOS_SUBDIR)) {
            warn!(error = %e, "undo: rm -rf repos fallito, proseguo (best-effort)");
        }

        // Contenitore <install_dir>: rimuovilo SOLO se vuoto (gli undo di venv e
        // config sono già girati prima, in ordine inverso). Se contiene altro
        // — anche materiale preesistente — lo lasciamo intatto.
        match self.ops.dir_is_empty(&ctx.install_dir) {
            Ok(true) => {
                if let Err(e) = self.ops.rmdir(&ctx.install_dir) {
                    warn!(error = %e, "undo: rimozione contenitore fallita, proseguo");
                } else {
                    info!(dir = %ctx.install_dir.display(), "undo: contenitore install_dir rimosso (era vuoto)");
                }
            }
            Ok(false) => {
                info!(dir = %ctx.install_dir.display(), "undo: contenitore non vuoto, lo lascio");
            }
            Err(e) => warn!(error = %e, "undo: impossibile verificare il contenitore, non rimuovo"),
        }
        Ok(())
    }

    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::to_value(&self.snap).unwrap_or(serde_json::Value::Null)
    }

    /// `had_invalid_dir` non è reidratato di proposito: governa il `run` (una
    /// dir preesistente non valida va rimossa prima di clonare), non l'undo, e
    /// non viene serializzato.
    fn rehydrate(&mut self, snapshot: &serde_json::Value) -> Result<(), StepError> {
        let snap = decode_snapshot(self.name(), snapshot)?;
        self.snap = snap;
        Ok(())
    }
}
