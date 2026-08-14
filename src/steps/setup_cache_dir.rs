//! [`SetupCacheDir`]: owns `<odoo_home>/.cache`, so the rollback can remove it.
//!
//! # why it exists (A-R5-3, second half)
//!
//! `/opt/odoo` is the `odoo` user's `$HOME`, which the installer marks
//! `Preexisting` and never empties. but several programs run inside it on our
//! behalf — `pip`, `odoo-bin`, the service — and they all write to
//! `$HOME/.cache`.
//!
//! R6 closed the biggest case by moving pip's cache into the venv. it was not
//! enough: the first CI run to reach **the end** of an installation found
//! `/opt/odoo/.cache` again after a complete rollback, because fontconfig and
//! pip's own version selfcheck write there too.
//!
//! chasing the producers is a losing battle — they are third-party programs
//! whose behaviour changes between versions. so the question changes: not "who
//! wrote into `.cache`" but "**whose** is `.cache`". if we create it, the
//! rollback removes it; if it was there, it is the customer's. the number of
//! producers becomes irrelevant.
//!
//! # compared with `SetupDataDir`
//!
//! same mechanics, **different gate**: the filestore holds application data, so
//! its removal also depends on owning the database. a cache is regenerable by
//! definition, and the only question is who created the directory.
//!
//! # position in the sequence
//!
//! early, before anything runs as `odoo`, or the snapshot would find a
//! "pre-existing" `.cache` that is nothing of the sort.
//!
//! the bonus is the undo's order: undos run backwards, so early here means
//! **late** there — the cache goes after the service is stopped, the venv
//! deleted and the sources removed, i.e. after every possible writer has
//! stopped. the opposite of the compromise `setup_data_dir` must accept.

use serde::{Deserialize, Serialize};
use tracing::info;

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};
use crate::system_ops::SystemOps;

/// the cache directory's name inside the `odoo` user's home.
const CACHE_SUBDIR: &str = ".cache";

/// the step's persisted snapshot.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheDirSnapshot {
    /// the cache's state before us.
    pub prestate: PreState,
    /// the highest missing level, hence the one we created. always the
    /// directory itself here, but persisted anyway: the undo decides what to
    /// remove by **re-reading** this, not by recomputing it.
    pub created_root: Option<std::path::PathBuf>,
}

/// creates, and therefore owns, `<odoo_home>/.cache`.
pub struct SetupCacheDir {
    ops: Box<dyn SystemOps>,
    snap: CacheDirSnapshot,
}

impl SetupCacheDir {
    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            snap: CacheDirSnapshot::default(),
        }
    }

    /// `<odoo_home>/.cache`.
    pub fn cache_dir(ctx: &Context) -> std::path::PathBuf {
        ctx.odoo_home.join(CACHE_SUBDIR)
    }
}

impl Step for SetupCacheDir {
    fn name(&self) -> &str {
        "setup-cache-dir"
    }

    fn snapshot(&mut self, ctx: &Context) -> Result<(), StepError> {
        let cache = Self::cache_dir(ctx);
        if self.ops.path_exists(&cache) {
            self.snap.prestate = PreState::Preexisting;
            self.snap.created_root = None;
        } else {
            self.snap.prestate = PreState::Untracked;
            self.snap.created_root =
                crate::steps::highest_missing_level(self.ops.as_ref(), &ctx.odoo_home, &cache);
        }
        info!(
            cache = %cache.display(),
            prestate = ?self.snap.prestate,
            "snapshot: setup-cache-dir"
        );
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        let cache = Self::cache_dir(ctx);

        if self.snap.prestate == PreState::Preexisting {
            info!(
                cache = %cache.display(),
                "run: the cache was already there, it is not ours (we neither use nor remove it)"
            );
            return Ok(());
        }
        if ctx.dry_run {
            info!(cache = %cache.display(), "run (dry run): would create the cache as the odoo user");
            return Ok(());
        }

        // created as `odoo`, because that is who will write there: root-owned,
        // the programs would open another cache somewhere else.
        self.ops.mkdir_p_as_user(&ctx.odoo_user, &cache)?;
        self.snap.prestate = PreState::CreatedByUs;
        info!(cache = %cache.display(), "run: cache created (owned {}:{})", ctx.odoo_user, ctx.odoo_user);
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        if self.snap.prestate != PreState::CreatedByUs {
            info!(
                prestate = ?self.snap.prestate,
                "undo NO-OP (cache not created by us)"
            );
            return Ok(());
        }

        // no gate beyond the `PreState`: a cache is regenerable and belongs to
        // no database, so the only question that matters was asked in the
        // snapshot.
        let target = self
            .snap
            .created_root
            .clone()
            .unwrap_or_else(|| Self::cache_dir(ctx));
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

    fn rehydrate(&mut self, snapshot: &serde_json::Value) -> Result<(), StepError> {
        let snap = decode_snapshot(self.name(), snapshot)?;
        self.snap = snap;
        Ok(())
    }
}
