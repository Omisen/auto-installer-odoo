//! [`CreateDatabase`]: crea il database applicativo, in modo reversibile.
//!
//! # Protezione critica: mai droppare un database preesistente
//!
//! Questa è la singola barriera che distingue "installer che pulisce" da
//! "installer che può distruggere i dati di un cliente". Un database con lo
//! stesso nome che **esisteva già** può contenere dati reali: l'undo lo droppa
//! **solo** se lo abbiamo creato noi (`PreState::CreatedByUs`). Su `Preexisting`
//! l'undo è rigorosamente NO-OP.
//!
//! Il ramo è reso impossibile da sbagliare: il `PreState` governa il drop, e la
//! chiamata a `dropdb` è raggiungibile solo nel ramo `CreatedByUs`.

use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::Step;
use crate::system_ops::{RealSystemOps, SystemOps};

/// Crea il database `db_name` (reversibile) con owner `db_user`.
pub struct CreateDatabase {
    ops: Box<dyn SystemOps>,
    prestate: PreState,
}

impl CreateDatabase {
    pub fn new() -> Self {
        Self::with_ops(Box::new(RealSystemOps::new()))
    }

    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            prestate: PreState::Untracked,
        }
    }
}

impl Default for CreateDatabase {
    fn default() -> Self {
        Self::new()
    }
}

impl Step for CreateDatabase {
    fn name(&self) -> &str {
        "create-database"
    }

    fn snapshot(&mut self, ctx: &Context) -> Result<(), StepError> {
        // Rileva se il DB esisteva PRIMA di noi: è la fonte di verità della
        // protezione anti-drop.
        self.prestate = if self.ops.pg_db_exists(&ctx.db_name)? {
            PreState::Preexisting
        } else {
            PreState::Untracked
        };
        info!(db = %ctx.db_name, prestate = ?self.prestate, "snapshot create-database");
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        if self.prestate == PreState::Preexisting {
            info!(db = %ctx.db_name, "run: database già presente, skip creazione");
            return Ok(());
        }
        if ctx.dry_run {
            info!(db = %ctx.db_name, owner = %ctx.db_user, "run (dry-run): createdb --owner");
            return Ok(());
        }
        self.ops.createdb(&ctx.db_user, &ctx.db_name)?;
        self.prestate = PreState::CreatedByUs;
        info!(db = %ctx.db_name, owner = %ctx.db_user, "run: database creato");
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        // PROTEZIONE CRITICA: si droppa SOLO ciò che abbiamo creato noi.
        // Un DB preesistente non viene MAI rimosso — potrebbe contenere dati
        // reali del cliente.
        if self.prestate != PreState::CreatedByUs {
            info!(
                db = %ctx.db_name,
                prestate = ?self.prestate,
                "undo NO-OP: database preesistente, NON rimosso (protezione dati cliente)"
            );
            return Ok(());
        }
        if ctx.dry_run {
            info!(db = %ctx.db_name, "undo (dry-run): dropdb --if-exists --force");
            return Ok(());
        }
        if let Err(e) = self.ops.dropdb(&ctx.db_name) {
            warn!(db = %ctx.db_name, error = %e, "undo: dropdb fallito, proseguo (best-effort)");
        }
        Ok(())
    }

    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::to_value(&self.prestate).unwrap_or(serde_json::Value::Null)
    }
}
