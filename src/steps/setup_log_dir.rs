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
            info!("snapshot: ODOO_LOGFILE disabilitato, step no-op");
            return Ok(());
        };
        let Some(dir) = logfile.parent() else {
            self.prestate = PreState::Untracked;
            warn!(logfile = %logfile.display(), "snapshot: logfile senza directory genitrice, step no-op");
            return Ok(());
        };

        self.prestate = if self.ops.path_exists(dir) {
            PreState::Preexisting
        } else {
            PreState::Untracked
        };
        info!(dir = %dir.display(), prestate = ?self.prestate, "snapshot setup-log-dir");
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        let Some(logfile) = &ctx.odoo_logfile else {
            info!("run: ODOO_LOGFILE disabilitato, nessuna azione");
            return Ok(());
        };
        let Some(dir) = logfile.parent() else {
            return Ok(());
        };
        let user = &ctx.odoo_user;

        // already there and not ours: left untouched.
        if self.prestate == PreState::Preexisting {
            info!(dir = %dir.display(), "run: log dir già presente, skip");
            return Ok(());
        }

        if ctx.dry_run {
            info!(dir = %dir.display(), "run (dry-run): creerei la log dir + chown {user}:{user} 0750");
            return Ok(());
        }

        self.ops.mkdir(dir)?;
        self.ops.chown_named(dir, user, user)?;
        self.ops.chmod(dir, LOG_DIR_MODE)?;

        self.prestate = PreState::CreatedByUs;
        info!(dir = %dir.display(), "run: log dir creata, owned {user}:{user} 0750");
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        if self.prestate != PreState::CreatedByUs {
            info!(prestate = ?self.prestate, "undo NO-OP (log dir non creata da noi)");
            return Ok(());
        }

        // being `CreatedByUs` guarantees the log file and its dir are set.
        let dir = match ctx.odoo_logfile.as_ref().and_then(|f| f.parent()) {
            Some(dir) => dir,
            None => return Ok(()),
        };

        if ctx.dry_run {
            info!(dir = %dir.display(), "undo (dry-run): rimuoverei la log dir se vuota");
            return Ok(());
        }

        // only if empty: never delete logs already written.
        match self.ops.dir_is_empty(dir) {
            Ok(true) => match self.ops.rmdir(dir) {
                Ok(()) => info!(dir = %dir.display(), "undo: log dir rimossa"),
                Err(e) => {
                    warn!(dir = %dir.display(), error = %e, "undo: rmdir fallito, proseguo (best-effort)")
                }
            },
            Ok(false) => {
                warn!(dir = %dir.display(), "undo: log dir non vuota, non la rimuovo (best-effort)")
            }
            Err(e) => {
                warn!(dir = %dir.display(), error = %e, "undo: impossibile verificare la log dir, non rimuovo")
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
