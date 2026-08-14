//! [`CreateDbRole`]: creates the PostgreSQL role for Odoo, reversibly.
//!
//! # coordinating with the database (reverse order)
//!
//! `DROP ROLE` fails if the role owns objects — and it owns the database
//! created by [`CreateDatabase`](crate::steps::create_database). the production
//! order is role then database, so the rollback drops the database **first**.
//! same pattern as the home coordination between the first two steps.
//!
//! the password arrives as a [`Secret`](crate::secret::Secret): the plaintext
//! is extracted only at the `SystemOps` call site and **never logged**, with
//! escaping and safe delivery inside the boundary.

use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};
use crate::system_ops::SystemOps;

/// creates the `db_user` role, reversibly.
pub struct CreateDbRole {
    ops: Box<dyn SystemOps>,
    prestate: PreState,
}

impl CreateDbRole {
    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            prestate: PreState::Untracked,
        }
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

        // extract the secret only here; empty means peer auth.
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
        // the role's database was already dropped, in the reverse order.
        if let Err(e) = self.ops.pg_drop_role(&ctx.db_user) {
            warn!(role = %ctx.db_user, error = %e, "undo: DROP ROLE fallito, proseguo (best-effort)");
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
