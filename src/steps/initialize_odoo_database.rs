//! [`InitializeOdooDatabase`]: initialises Odoo's base schema.
//!
//! # critical protection: a hard stop on a pre-existing database
//!
//! the twin of the anti-drop rule. together they guarantee that a database
//! holding a customer's real data is neither **deleted** nor **written into**.
//!
//! the installer REFUSES to initialise a database it did not create: writing
//! the schema into a pre-existing one has no clean undo, so the defence is not
//! to do it at all. "is the database ours?" arrives from `CreateDatabase`
//! through [`Context::db_created_by_us`], defaulting to refuse.
//!
//! # C2 — a non-atomic init, and a no-op undo
//!
//! the init is not atomic, but no incremental repair is attempted: since it
//! runs **only** on databases of ours, cleaning the schema up is covered by the
//! `dropdb` that runs after it in the reverse order. the database is thrown
//! away and recreated clean.

use std::sync::atomic::Ordering;

use tracing::info;

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};
use crate::system_ops::SystemOps;

const VENV_SUBDIR: &str = "sandbox";
const REPO_SUBDIR: &str = "odoo";

/// initialises Odoo's base schema, only on a database of ours.
pub struct InitializeOdooDatabase {
    ops: Box<dyn SystemOps>,
    prestate: PreState,
}

impl InitializeOdooDatabase {
    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            prestate: PreState::Untracked,
        }
    }

    fn python_bin(ctx: &Context) -> std::path::PathBuf {
        ctx.install_dir
            .join(VENV_SUBDIR)
            .join("bin")
            .join("python3")
    }
    fn odoo_bin(ctx: &Context) -> std::path::PathBuf {
        ctx.install_dir.join(REPO_SUBDIR).join("odoo-bin")
    }
    fn conf(ctx: &Context) -> std::path::PathBuf {
        ctx.install_dir
            .join(format!("odoo{}.conf", ctx.odoo_version_short))
    }
}

impl Step for InitializeOdooDatabase {
    fn name(&self) -> &str {
        "initialize-odoo-database"
    }

    fn snapshot(&mut self, ctx: &Context) -> Result<(), StepError> {
        // schema already there means nothing to do, whoever owns the database.
        if self.ops.pg_db_initialized(&ctx.db_name)? {
            self.prestate = PreState::Preexisting;
            info!(db = %ctx.db_name, "snapshot: the Odoo schema is already there");
            return Ok(());
        }

        // schema absent. HARD STOP when the database is not ours.
        //
        // the installer cannot itself tell "our leftover" from "the customer's
        // database": in this run it simply exists. the user can, and has the
        // tool — if the leftover came from an interrupted installation, its
        // state file is still on disk and `invok rollback` consumes it,
        // removing exactly what that run created (A3.3).
        if !ctx.db_created_by_us.load(Ordering::SeqCst) {
            return Err(StepError::Precondition(format!(
                "database '{db}' already existed before the installation; initialisation of \
                 the Odoo schema is refused so pre-existing data is not altered. use a \
                 different DB name, or an empty DB created by the installer.\n\
                 if '{db}' is the leftover of an earlier, unfinished installation (and not a \
                 database with real data), clean that installation up with \
                 `sudo invok rollback`: it reads the state that run left behind and removes \
                 only what it created. otherwise remove the database by hand — \
                 `sudo -u postgres dropdb {db}` — or choose a different name.",
                db = ctx.db_name
            )));
        }

        // ours, and empty: we will proceed.
        self.prestate = PreState::Untracked;
        info!(db = %ctx.db_name, "snapshot: the DB is ours, the schema needs initialising");
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        if self.prestate == PreState::Preexisting {
            info!(db = %ctx.db_name, "run: schema already there, skipping the init");
            return Ok(());
        }
        if ctx.dry_run {
            info!(db = %ctx.db_name, "run (dry run): odoo-bin -i base --without-demo=all --stop-after-init");
            return Ok(());
        }

        // initialise as the `odoo` user, not root.
        self.ops.odoo_init_base(
            &ctx.odoo_user,
            &Self::python_bin(ctx),
            &Self::odoo_bin(ctx),
            &Self::conf(ctx),
            &ctx.db_name,
        )?;

        // post-init check: the schema must now be there.
        if !self.ops.pg_db_initialized(&ctx.db_name)? {
            return Err(StepError::Precondition(format!(
                "initialisation of database '{}' failed: the base schema was not detected",
                ctx.db_name
            )));
        }

        self.prestate = PreState::CreatedByUs;
        info!(db = %ctx.db_name, "run: base schema initialised");
        Ok(())
    }

    fn undo(&self, _ctx: &Context) -> Result<(), StepError> {
        // a deliberate no-op (C2): the init runs only on databases of ours, and
        // the `dropdb` that follows covers the schema.
        info!(
            "undo NO-OP: the schema lives in a DB we created, and its removal is covered \
             by CreateDatabase's undo (a dropdb of the whole DB)"
        );
        Ok(())
    }

    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::to_value(&self.prestate).unwrap_or(serde_json::Value::Null)
    }

    /// rehydrated for symmetry even though the undo is a no-op: the contract
    /// holds for every step, so a future undo cannot be born with empty state
    /// and nobody noticing.
    fn rehydrate(&mut self, snapshot: &serde_json::Value) -> Result<(), StepError> {
        let prestate = decode_snapshot(self.name(), snapshot)?;
        self.prestate = prestate;
        Ok(())
    }
}
