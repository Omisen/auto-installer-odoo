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
            info!(db = %ctx.db_name, "snapshot: schema Odoo già presente");
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
                "Il database '{db}' esisteva già prima dell'installazione; \
                 l'inizializzazione dello schema Odoo è rifiutata per non alterare \
                 dati preesistenti. Usa un nome DB diverso o un DB vuoto creato \
                 dall'installer.\n\
                 Se '{db}' è il residuo di un'installazione precedente non completata \
                 (e non un database con dati reali), ripulisci quella installazione con \
                 `sudo invok rollback`: legge lo stato lasciato da quella \
                 esecuzione e rimuove solo ciò che aveva creato lei. In alternativa \
                 rimuovi il database a mano — `sudo -u postgres dropdb {db}` — oppure \
                 scegli un nome diverso.",
                db = ctx.db_name
            )));
        }

        // ours, and empty: we will proceed.
        self.prestate = PreState::Untracked;
        info!(db = %ctx.db_name, "snapshot: DB nostro, schema da inizializzare");
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        if self.prestate == PreState::Preexisting {
            info!(db = %ctx.db_name, "run: schema già presente, skip init");
            return Ok(());
        }
        if ctx.dry_run {
            info!(db = %ctx.db_name, "run (dry-run): odoo-bin -i base --without-demo=all --stop-after-init");
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
                "inizializzazione del database '{}' fallita: schema base non rilevato",
                ctx.db_name
            )));
        }

        self.prestate = PreState::CreatedByUs;
        info!(db = %ctx.db_name, "run: schema base inizializzato");
        Ok(())
    }

    fn undo(&self, _ctx: &Context) -> Result<(), StepError> {
        // a deliberate no-op (C2): the init runs only on databases of ours, and
        // the `dropdb` that follows covers the schema.
        info!(
            "undo NO-OP: lo schema vive in un DB CreatedByUs; la sua rimozione è \
             coperta dall'undo di CreateDatabase (dropdb dell'intero DB)"
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
