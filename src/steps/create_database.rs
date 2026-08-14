//! [`CreateDatabase`]: creates the application database, reversibly.
//!
//! # critical protection: never drop a pre-existing database
//!
//! the single barrier separating "an installer that cleans up" from "an
//! installer that can destroy a customer's data". a database that **already
//! existed** under the same name may hold real data, so the undo drops it
//! **only** when we created it; on `Preexisting` it is strictly a no-op.
//!
//! the branch is made impossible to get wrong: the `PreState` governs the drop,
//! and the call is reachable only inside the `CreatedByUs` branch.

use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};
use crate::system_ops::SystemOps;

/// creates the `db_name` database, reversibly, owned by `db_user`.
pub struct CreateDatabase {
    ops: Box<dyn SystemOps>,
    prestate: PreState,
}

impl CreateDatabase {
    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            prestate: PreState::Untracked,
        }
    }
}

impl Step for CreateDatabase {
    fn name(&self) -> &str {
        "create-database"
    }

    fn snapshot(&mut self, ctx: &Context) -> Result<(), StepError> {
        // did the database exist BEFORE us? the anti-drop protection's source
        // of truth.
        self.prestate = if self.ops.pg_db_exists(&ctx.db_name)? {
            PreState::Preexisting
        } else {
            PreState::Untracked
        };

        // publish the verdict for the init step: writing the schema is allowed
        // ONLY into a database that is not pre-existing.
        ctx.db_created_by_us.store(
            self.prestate != PreState::Preexisting,
            std::sync::atomic::Ordering::SeqCst,
        );

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
        // CRITICAL PROTECTION: drop ONLY what we created. a pre-existing
        // database is NEVER removed — it may hold the customer's real data.
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

    /// **here** is where the anti-drop protection crosses the disk boundary.
    ///
    /// the persisted `PreState` is the only thing that, in a rollback run days
    /// later, tells "a database we created" from "the customer's, under the
    /// same name". it is not recomputed by asking PostgreSQL — by then the
    /// database exists either way and the question has no answer. an unreadable
    /// value is an error that blocks the undo: better a database left to remove
    /// by hand than one dropped on a wrong inference.
    fn rehydrate(&mut self, snapshot: &serde_json::Value) -> Result<(), StepError> {
        let prestate = decode_snapshot(self.name(), snapshot)?;
        self.prestate = prestate;
        Ok(())
    }
}
