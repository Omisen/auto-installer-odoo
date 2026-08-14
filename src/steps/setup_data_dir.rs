//! [`SetupDataDir`]: creates Odoo's `data_dir` — the **filestore** —
//! reversibly, under the same anti-drop protection as the database.
//!
//! # why it exists (A-R5-3)
//!
//! the `data_dir` is where Odoo writes the actual files of record attachments.
//! while no step created it, that directory came into existence **by itself**
//! on Odoo's first start, inside a `/opt/odoo` the installer marks
//! `Preexisting` and never touches. measured result: after a complete rollback,
//! `/opt/odoo/.local` was still there.
//!
//! an artifact born unrecorded cannot be undone. created by a step, with its
//! own `PreState`, it becomes removable by the only route this project allows.
//!
//! # the filestore follows the database's fate (anti-drop)
//!
//! a filestore is not a cache: it is the on-disk half of the application data.
//! if the database was **pre-existing** — the customer's, which
//! `CreateDatabase` protects from the drop — its filestore holds real
//! attachments, whoever created the directory. so the undo needs **two**
//! conditions: `CreatedByUs` **and** a database of ours.
//!
//! the second is read from `Context::db_created_by_us` during the snapshot,
//! which runs after `CreateDatabase`'s, and is then **persisted** here. not
//! redundancy: a rollback from disk rebuilds the `Context` from the persisted
//! config, where that flag defaults to `false`, so without a local copy the
//! undo would never know it may act. the verdict is re-read, not re-derived.
//!
//! # undo ordering, declared
//!
//! this step comes after `create-database`, so its undo runs **before** the
//! `dropdb`. if that failed — it is best-effort — a database of ours would be
//! left without its filestore. unavoidable: this snapshot must run *after*
//! `CreateDatabase`'s to know whose the database is, and the case is a database
//! we are throwing away anyway.

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};
use crate::steps::generate_config;
use crate::system_ops::SystemOps;

/// the step's persisted snapshot.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataDirSnapshot {
    /// the `data_dir`'s state before us.
    pub prestate: PreState,
    /// the **highest** level missing under `odoo_home`, hence the one we
    /// created. that is what the undo removes: with a pre-existing `.local`
    /// this holds `.local/share`, or the filestore alone, and the customer's
    /// `.local` is never touched.
    pub created_root: Option<std::path::PathBuf>,
    /// was the database ours? the undo's anti-drop condition.
    pub db_was_ours: bool,
}

/// creates Odoo's `data_dir`, reversibly, gated on the database's anti-drop.
pub struct SetupDataDir {
    ops: Box<dyn SystemOps>,
    snap: DataDirSnapshot,
}

impl SetupDataDir {
    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            snap: DataDirSnapshot::default(),
        }
    }

    /// the root of what our `mkdir -p` will create, shared with
    /// [`setup_cache_dir`](crate::steps::setup_cache_dir).
    fn highest_missing_level(&self, ctx: &Context) -> Option<std::path::PathBuf> {
        crate::steps::highest_missing_level(
            self.ops.as_ref(),
            &ctx.odoo_home,
            &generate_config::data_dir(ctx),
        )
    }
}

impl Step for SetupDataDir {
    fn name(&self) -> &str {
        "setup-data-dir"
    }

    fn snapshot(&mut self, ctx: &Context) -> Result<(), StepError> {
        let data_dir = generate_config::data_dir(ctx);

        // read NOW: `CreateDatabase::snapshot` has published it, and from here
        // it lives in our own persisted snapshot.
        self.snap.db_was_ours = ctx
            .db_created_by_us
            .load(std::sync::atomic::Ordering::SeqCst);

        if self.ops.path_exists(&data_dir) {
            self.snap.prestate = PreState::Preexisting;
            self.snap.created_root = None;
        } else {
            self.snap.prestate = PreState::Untracked;
            self.snap.created_root = self.highest_missing_level(ctx);
        }

        info!(
            data_dir = %data_dir.display(),
            prestate = ?self.snap.prestate,
            created_root = ?self.snap.created_root,
            db_was_ours = self.snap.db_was_ours,
            "snapshot: setup-data-dir"
        );
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        let data_dir = generate_config::data_dir(ctx);

        if self.snap.prestate == PreState::Preexisting {
            info!(
                data_dir = %data_dir.display(),
                "run: filestore already there, nothing to do (it is not ours)"
            );
            return Ok(());
        }
        if ctx.dry_run {
            info!(
                data_dir = %data_dir.display(),
                "run (dry run): would create the data_dir as the odoo user"
            );
            return Ok(());
        }

        // created as `odoo`: the service has to be able to write there, and
        // `mkdir -p` covers the intermediate levels.
        self.ops.mkdir_p_as_user(&ctx.odoo_user, &data_dir)?;
        self.snap.prestate = PreState::CreatedByUs;
        info!(
            data_dir = %data_dir.display(),
            "run: data_dir created (owned {}:{})",
            ctx.odoo_user, ctx.odoo_user
        );
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        if self.snap.prestate != PreState::CreatedByUs {
            info!(
                prestate = ?self.snap.prestate,
                "undo NO-OP (data_dir not created by us)"
            );
            return Ok(());
        }

        // CRITICAL PROTECTION: a pre-existing database's filestore holds the
        // customer's attachments. we created the directory, not the data.
        if !self.snap.db_was_ours {
            warn!(
                "undo NO-OP: the database was pre-existing, so the filestore holds the \
                 customer's attachments and is NOT removed (customer data protection)"
            );
            return Ok(());
        }

        let target = self
            .snap
            .created_root
            .clone()
            .unwrap_or_else(|| generate_config::data_dir(ctx));

        // the directory is ours and the data belongs to a database we are
        // dropping. the perimeter net on the rehydrated path is in the helper.
        crate::steps::remove_created_root(
            self.ops.as_ref(),
            self.name(),
            &ctx.odoo_home,
            &target,
            ctx.dry_run,
        );
        Ok(())
    }

    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::to_value(&self.snap).unwrap_or(serde_json::Value::Null)
    }

    /// rehydrates `created_root` **and** `db_was_ours`: the first says *what*
    /// to remove, the second *whether* that is allowed. recomputing either
    /// after the installation would answer wrongly — the directories now exist,
    /// and so does the database whoever created it.
    fn rehydrate(&mut self, snapshot: &serde_json::Value) -> Result<(), StepError> {
        let snap = decode_snapshot(self.name(), snapshot)?;
        self.snap = snap;
        Ok(())
    }
}
