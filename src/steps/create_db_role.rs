//! [`CreateDbRole`]: crea il ruolo PostgreSQL per Odoo, in modo reversibile.
//!
//! # Coordinamento con il database (ordine inverso)
//!
//! `DROP ROLE` fallisce se il ruolo possiede oggetti — e il ruolo possiede il
//! database creato da [`CreateDatabase`](crate::steps::create_database). La
//! sequenza di produzione è `CreateDbRole → CreateDatabase`, quindi al rollback
//! l'ordine inverso esegue **prima** l'undo di `CreateDatabase` (drop del DB) e
//! **poi** l'undo di `CreateDbRole` (drop del ruolo). Il DB sparisce prima del
//! ruolo che lo possiede. Stesso pattern del coordinamento home in Fase 3.
//!
//! La password del ruolo arriva dal tipo [`Secret`](crate::secret::Secret): il
//! valore in chiaro è estratto solo al punto di chiamata del confine
//! `SystemOps` e **non viene mai loggato**; l'escaping e l'invio sicuro
//! avvengono dentro `pg_create_role`.

use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::Step;
use crate::system_ops::{RealSystemOps, SystemOps};

/// Crea il ruolo `db_user` (reversibile).
pub struct CreateDbRole {
    ops: Box<dyn SystemOps>,
    prestate: PreState,
}

impl CreateDbRole {
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

impl Default for CreateDbRole {
    fn default() -> Self {
        Self::new()
    }
}

impl Step for CreateDbRole {
    fn name(&self) -> &str {
        "create-db-role"
    }

    fn snapshot(&mut self, ctx: &Context) -> Result<(), StepError> {
        self.prestate = if self.ops.pg_role_exists(&ctx.db_user)? {
            PreState::Preexisting
        } else {
            PreState::Untracked
        };
        info!(role = %ctx.db_user, prestate = ?self.prestate, "snapshot create-db-role");
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        if self.prestate == PreState::Preexisting {
            info!(role = %ctx.db_user, "run: ruolo già presente, skip creazione");
            return Ok(());
        }
        if ctx.dry_run {
            info!(role = %ctx.db_user, "run (dry-run): CREATE ROLE ... WITH LOGIN CREATEDB");
            return Ok(());
        }

        // Estrai il segreto solo qui; peer auth se vuoto. Mai loggare la password.
        let password = if ctx.db_password.is_empty() {
            None
        } else {
            Some(ctx.db_password.expose())
        };
        self.ops.pg_create_role(&ctx.db_user, password)?;

        self.prestate = PreState::CreatedByUs;
        info!(role = %ctx.db_user, with_password = password.is_some(), "run: ruolo creato");
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        if self.prestate != PreState::CreatedByUs {
            info!(role = %ctx.db_user, prestate = ?self.prestate, "undo NO-OP (ruolo non creato da noi)");
            return Ok(());
        }
        if ctx.dry_run {
            info!(role = %ctx.db_user, "undo (dry-run): DROP ROLE");
            return Ok(());
        }
        // Il DB del ruolo è già stato droppato da CreateDatabase.undo (ordine inverso).
        if let Err(e) = self.ops.pg_drop_role(&ctx.db_user) {
            warn!(role = %ctx.db_user, error = %e, "undo: DROP ROLE fallito, proseguo (best-effort)");
        }
        Ok(())
    }

    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::to_value(&self.prestate).unwrap_or(serde_json::Value::Null)
    }
}
