//! [`SetupLogDir`]: creates the log file's directory, **only if**
//! `ODOO_LOGFILE` is set.
//!
//! by default Odoo logs to journal/stdout, so there is nothing to create and
//! the step is entirely `Untracked`.
//!
//! runs **after** [`CreateOdooUser`](crate::steps::create_odoo_user), so the
//! directory can be chowned to a user that exists.

use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};
use crate::system_ops::SystemOps;

/// log directory permissions.
const LOG_DIR_MODE: u32 = 0o750;

/// creates the log directory, reversibly, when `ODOO_LOGFILE` is set.
pub struct SetupLogDir {
    ops: Box<dyn SystemOps>,
    prestate: PreState,
}

impl SetupLogDir {
    /// constructor with injectable `SystemOps`, for the tests.
    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            prestate: PreState::Untracked,
        }
    }
}

impl Step for SetupLogDir {
    fn name(&self) -> &str {
        "setup-log-dir"
    }

    fn snapshot(&mut self, ctx: &Context) -> Result<(), StepError> {
        let Some(logfile) = &ctx.odoo_logfile else {
            self.prestate = PreState::Untracked;
            info!("snapshot: ODOO_LOGFILE is disabled, the step is a no-op");
            return Ok(());
        };
        let Some(dir) = logfile.parent() else {
            self.prestate = PreState::Untracked;
            warn!(logfile = %logfile.display(), "snapshot: the logfile has no parent directory, the step is a no-op");
            return Ok(());
        };

        self.prestate = if self.ops.path_exists(dir) {
            PreState::Preexisting
        } else {
            PreState::Untracked
        };
        info!(dir = %dir.display(), prestate = ?self.prestate, "snapshot: setup-log-dir");
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        let Some(logfile) = &ctx.odoo_logfile else {
            info!("run: ODOO_LOGFILE is disabled, nothing to do");
            return Ok(());
        };
        let Some(dir) = logfile.parent() else {
            return Ok(());
        };
        let user = &ctx.odoo_user;

        // already there and not ours: left untouched.
        if self.prestate == PreState::Preexisting {
            info!(dir = %dir.display(), "run: the log dir is already there, skipping");
            return Ok(());
        }

        if ctx.dry_run {
            info!(dir = %dir.display(), "run (dry run): would create the log dir and chown {user}:{user} 0750");
            return Ok(());
        }

        self.ops.mkdir(dir)?;
        self.ops.chown_named(dir, user, user)?;
        self.ops.chmod(dir, LOG_DIR_MODE)?;

        self.prestate = PreState::CreatedByUs;
        info!(dir = %dir.display(), "run: log dir created, owned {user}:{user} 0750");
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        if self.prestate != PreState::CreatedByUs {
            info!(prestate = ?self.prestate, "undo NO-OP (log dir not created by us)");
            return Ok(());
        }

        // being `CreatedByUs` guarantees the log file and its dir are set.
        let dir = match ctx.odoo_logfile.as_ref().and_then(|f| f.parent()) {
            Some(dir) => dir,
            None => return Ok(()),
        };

        if ctx.dry_run {
            info!(dir = %dir.display(), "undo (dry run): would remove the log dir if it is empty");
            return Ok(());
        }

        // only if empty: never delete logs already written.
        match self.ops.dir_is_empty(dir) {
            Ok(true) => match self.ops.rmdir(dir) {
                Ok(()) => info!(dir = %dir.display(), "undo: log dir removed"),
                Err(e) => {
                    warn!(dir = %dir.display(), error = %e, "undo: rmdir failed, proceeding (best-effort)")
                }
            },
            Ok(false) => {
                warn!(dir = %dir.display(), "undo: the log dir is not empty, leaving it (best-effort)")
            }
            Err(e) => {
                warn!(dir = %dir.display(), error = %e, "undo: cannot inspect the log dir, not removing it")
            }
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
