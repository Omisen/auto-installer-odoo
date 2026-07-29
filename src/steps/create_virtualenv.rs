//! [`CreateVirtualenv`] (9b): crea il virtualenv Python in `<install_dir>/sandbox`.
//!
//! Tutte le operazioni come utente **odoo** (privilegio minimo). Il suo undo
//! (`rm -rf sandbox`) copre anche i pacchetti pip di
//! [`InstallPythonRequirements`](crate::steps::install_python_requirements):
//! rimuovendo il venv spariscono tutti i pacchetti al suo interno.

use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};
use crate::system_ops::{RealSystemOps, SystemOps};

const VENV_SUBDIR: &str = "sandbox";

/// Crea il virtualenv (reversibile).
pub struct CreateVirtualenv {
    ops: Box<dyn SystemOps>,
    prestate: PreState,
}

impl CreateVirtualenv {
    pub fn new() -> Self {
        Self::with_ops(Box::new(RealSystemOps::new()))
    }

    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            prestate: PreState::Untracked,
        }
    }

    fn venv_dir(ctx: &Context) -> std::path::PathBuf {
        ctx.install_dir.join(VENV_SUBDIR)
    }
}

impl Default for CreateVirtualenv {
    fn default() -> Self {
        Self::new()
    }
}

impl Step for CreateVirtualenv {
    fn name(&self) -> &str {
        "create-virtualenv"
    }

    fn snapshot(&mut self, ctx: &Context) -> Result<(), StepError> {
        let venv = Self::venv_dir(ctx);
        self.prestate = if self.ops.venv_python_exists(&venv) {
            PreState::Preexisting
        } else {
            PreState::Untracked
        };
        info!(venv = %venv.display(), prestate = ?self.prestate, "snapshot create-virtualenv");
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        if self.prestate == PreState::Preexisting {
            info!("run: virtualenv già presente, skip");
            return Ok(());
        }
        if ctx.dry_run {
            info!("run (dry-run): python3 -m venv <install_dir>/sandbox");
            return Ok(());
        }

        // python3-venv deve esserci (installato dalle dipendenze di Fase 4).
        //
        // La domanda è "il sistema sa creare un virtualenv?", e si risolve in
        // `import ensurepip` — non nella presenza del modulo `venv`, che è nella
        // stdlib e c'è sempre. Vedi
        // [`SystemOps::python_venv_available`](crate::system_ops::SystemOps::python_venv_available):
        // finché questa precondizione chiedeva del modulo sbagliato non poteva
        // fallire, e un sistema senza `python3-venv` arrivava fino al
        // `python3 -m venv`, che si ferma a metà lasciando una `sandbox`
        // incompleta (senza `bin/python`) e un errore grezzo di Python (A-R6-1).
        if !self.ops.python_venv_available() {
            return Err(StepError::Precondition(
                "impossibile creare un virtualenv: manca il modulo 'ensurepip', che su \
                 Debian/Ubuntu arriva col pacchetto python3-venv (o la sua variante \
                 versionata, es. python3.12-venv). Lo step install-system-dependencies \
                 dovrebbe averlo installato: controlla il suo esito nel log"
                    .to_string(),
            ));
        }

        let venv = Self::venv_dir(ctx);
        self.ops.create_venv(&ctx.odoo_user, &venv)?;
        self.prestate = PreState::CreatedByUs;
        info!(venv = %venv.display(), "run: virtualenv creato");
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        if self.prestate != PreState::CreatedByUs {
            info!(prestate = ?self.prestate, "undo NO-OP (venv non creato da noi)");
            return Ok(());
        }
        if ctx.dry_run {
            info!("undo (dry-run): rm -rf del virtualenv");
            return Ok(());
        }
        // rm -rf del venv: rimuove anche tutti i pacchetti pip (undo di 9c).
        let venv = Self::venv_dir(ctx);
        if let Err(e) = self.ops.remove_dir_all(&venv) {
            warn!(error = %e, "undo: rm -rf virtualenv fallito, proseguo (best-effort)");
        }
        Ok(())
    }

    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::to_value(&self.prestate).unwrap_or(serde_json::Value::Null)
    }

    fn rehydrate(&mut self, snapshot: &serde_json::Value) -> Result<(), StepError> {
        let prestate = decode_snapshot(self.name(), snapshot)?;
        self.prestate = prestate;
        Ok(())
    }
}
